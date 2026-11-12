//! Turns terminal input into Neovim API calls: keys, pastes, focus and mouse.

use std::io;
use std::time::{Duration, Instant};

use super::msgpack;
use super::nvim::Nvim;
use crate::term::input::{self, Event, MouseAction, MouseButton};

/// What can go wrong forwarding input: Neovim's pipe failing, or a paste too
/// long for a msgpack string.
#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    TooLong,
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::TooLong => write!(f, "TooLong"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Room for the keys of one read.
const KEYS_CAPACITY: usize = 4096;

/// A terminal's focus report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusReport {
    In,
    Out,
}

/// What input does that takes focus to its pane (`Forwarder::taking`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Taking {
    Nothing,
    /// Keys, pastes, or anything else typed.
    Key,
    /// A mouse button going down, not the wheel's.
    Click,
}

pub(crate) struct Forwarder {
    pub parser: input::Parser,
    /// The next `nvim_paste` phase: 1 starts a paste, 2 continues it.
    pub paste_phase: i64,
    /// The last paste chunk ended in CR: an LF starting the next chunk is the
    /// other half of that CRLF, not a second line break.
    pub paste_ended_in_cr: bool,
    /// For the debug commands; the real editor gives Ctrl+Q to Neovim.
    pub ctrl_q_quits: bool,
    /// The grid mouse events land on; 0 without ext_multigrid.
    pub grid: u32,
    /// Rows at the top showing the screen's tabline from column `tabline_col`:
    /// mouse events there go to grid 1, and below them move up past them.
    pub tabline_rows: u16,
    pub tabline_col: u16,
    /// Floats drawn over the terminal, below the tabline, lowest first: mouse
    /// events on one go to its grid. The first `targets_len` are in use.
    pub targets: [Target; MAX_TARGETS],
    pub targets_len: u8,
    /// The windows of a pane layout as drawn, below the tabline, each without
    /// a statusline neorex draws: mouse events under the floats go to the one
    /// under them; off them (on a separator, say) nowhere, but below them all,
    /// on the messages, to `grid`. Empty for a pane showing one window, which
    /// `grid` takes them all for.
    pub windows: Vec<Target>,
    /// The float or window a button went down on: its drags and release go
    /// there too.
    pub grab: Option<Target>,
    /// Off when the caller tells Neovim about focus itself, from `focused`.
    pub send_focus: bool,
    /// The last focus report seen.
    pub focused: Option<bool>,
    /// When the read that left an ESC waiting ended, if one is waiting.
    pub escape_since: Option<Instant>,
}

impl Default for Forwarder {
    fn default() -> Self {
        Forwarder {
            parser: input::Parser::default(),
            paste_phase: 1,
            paste_ended_in_cr: false,
            ctrl_q_quits: true,
            grid: 0,
            tabline_rows: 0,
            tabline_col: 0,
            targets: [Target::default(); MAX_TARGETS],
            targets_len: 0,
            windows: Vec::new(),
            grab: None,
            send_focus: true,
            focused: None,
            escape_since: None,
        }
    }
}

/// A float, or rows of a window of a pane layout, as drawn: its grid, and
/// where it's drawn, in terminal cells below the tabline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Target {
    pub grid: u32,
    pub row: i32,
    pub col: i32,
    pub width: u32,
    pub height: u32,
    /// The grid row its top row shows: 0 but for a window's last row
    /// showing its statusline under rows cut for want of room.
    pub grid_row: u32,
}

pub(crate) const MAX_TARGETS: usize = 16;

/// The grid a mouse event lands on, and where on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    grid: u32,
    row: u16,
    col: u16,
}

/// What `feed` and `feed_key` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fed {
    /// False if the user quit.
    pub more: bool,
    /// How many bytes were read: all, unless `feed_key` stopped after a key.
    pub read: usize,
    /// `feed_key` stopped after a key, which it sent.
    pub stopped_after_key: bool,
    /// Whether keys went to Neovim with `nvim_input`.
    pub sent_input: bool,
}

impl Forwarder {
    pub(crate) fn set_targets(&mut self, targets: &[Target]) {
        let n = targets.len().min(MAX_TARGETS);
        self.targets[..n].copy_from_slice(&targets[..n]);
        self.targets_len = n as u8;
    }

    pub(crate) fn set_windows(&mut self, windows: impl IntoIterator<Item = Target>) {
        self.windows.clear();
        self.windows.extend(windows);
    }

    /// The grid a mouse event lands on, and where on it; None off every
    /// window of a pane layout.
    fn route(&mut self, event: input::Mouse) -> Option<Route> {
        let held = event.action == MouseAction::Drag || event.action == MouseAction::Release;
        if event.row < self.tabline_rows && !(held && self.grab.is_some()) {
            if event.action == MouseAction::Release {
                self.grab = None;
            }
            return Some(Route {
                grid: 1,
                row: event.row,
                col: event.col.saturating_add(self.tabline_col),
            });
        }
        let mut m = event;
        m.row = m.row.saturating_sub(self.tabline_rows);
        let hit: Option<Target> = if held && self.grab.is_some() {
            self.grab
        } else {
            let (row, col) = (m.row as i64, m.col as i64);
            let under = |t: &Target| {
                row >= t.row as i64
                    && row < t.row as i64 + t.height as i64
                    && col >= t.col as i64
                    && col < t.col as i64 + t.width as i64
            };
            (0..self.targets_len as usize)
                .map(|j| self.targets[self.targets_len as usize - 1 - j])
                .find(under)
                .or_else(|| self.windows.iter().copied().find(under))
        };
        if m.action == MouseAction::Press
            && m.button != MouseButton::WheelUp
            && m.button != MouseButton::WheelDown
        {
            self.grab = hit;
        }
        if m.action == MouseAction::Release {
            self.grab = None;
        }
        let Some(t) = hit else {
            // between windows, not below them all
            let bottom = self.windows.iter().map(|t| t.row + t.height as i32).max();
            if bottom.is_some_and(|b| i32::from(m.row) < b) {
                return None;
            }
            return Some(Route {
                grid: self.grid,
                row: m.row,
                col: m.col,
            });
        };
        Some(Route {
            grid: t.grid,
            row: (m.row as i64 - t.row as i64 + t.grid_row as i64).clamp(0, u16::MAX as i64) as u16,
            col: (m.col as i64 - t.col as i64).clamp(0, u16::MAX as i64) as u16,
        })
    }

    /// Sends the input in `bytes` to Neovim and flushes. `more` is false if
    /// the user pressed Ctrl+Q to quit and `ctrl_q_quits` is set.
    pub(crate) fn feed(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<Fed> {
        self.read(nvim, bytes, false)
    }

    /// Begins a paste: the next chunk starts it.
    fn start_paste(&mut self) {
        self.paste_phase = 1;
        self.paste_ended_in_cr = false;
    }

    /// Like `feed`, but stops after the first key: what follows it is left for
    /// a later `feed`. Doesn't quit.
    pub(crate) fn feed_key(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<Fed> {
        self.read(nvim, bytes, true)
    }

    /// Reads `bytes` as `feed` does, but sends nothing: for input whose window
    /// is gone. Keeps what the forwarder remembers of the input, such as focus,
    /// and closes a paste that was begun.
    pub(crate) fn discard(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<()> {
        self.parser.feed(bytes);
        while let Some(event) = self.parser.next() {
            match event {
                Event::Focus(focused) => self.focused = Some(focused),
                Event::PasteStart => self.start_paste(),
                Event::PasteEnd if self.paste_phase == 2 => paste(nvim, b"", 3)?,
                _ => {}
            }
        }
        self.note_escape();
        nvim.flush()?;
        Ok(())
    }

    /// Whether `bytes`, read as `feed` reads them, are typed or clicked into
    /// the pane, as opposed to what only reports or looks: focus reports, and
    /// mouse events other than a button going down, such as the wheel and a
    /// pointer moving. Changes nothing: the parser is read from a copy.
    #[cfg(test)]
    pub(crate) fn takes_focus(&self, bytes: &[u8]) -> bool {
        self.taking(bytes) != Taking::Nothing
    }

    /// What in `bytes`, read as `feed` reads them, takes focus: a click, if
    /// any, else a key, a paste or what can't be read. Changes nothing.
    pub(crate) fn taking(&self, bytes: &[u8]) -> Taking {
        let mut parser = self.parser.clone();
        parser.feed(bytes);
        let mut taking = Taking::Nothing;
        while let Some(event) = parser.next() {
            match event {
                Event::Focus(_) => {}
                Event::Mouse(m) => match m.action {
                    MouseAction::Press => match m.button {
                        MouseButton::WheelUp
                        | MouseButton::WheelDown
                        | MouseButton::WheelLeft
                        | MouseButton::WheelRight => {}
                        _ => return Taking::Click,
                    },
                    MouseAction::Release | MouseAction::Drag | MouseAction::Motion => {}
                },
                Event::Key(_)
                | Event::PasteStart
                | Event::Paste(_)
                | Event::PasteEnd
                | Event::Unknown(_) => taking = Taking::Key,
            }
        }
        // an ESC left waiting may be the Escape key
        if parser.flush_due() {
            taking = Taking::Key;
        }
        taking
    }

    /// The last focus report in `bytes`, read as `feed` reads them. Changes
    /// nothing.
    pub(crate) fn focus_report(&self, bytes: &[u8]) -> Option<FocusReport> {
        let mut parser = self.parser.clone();
        parser.feed(bytes);
        let mut report = None;
        while let Some(event) = parser.next() {
            if let Event::Focus(f) = event {
                report = Some(if f { FocusReport::In } else { FocusReport::Out });
            }
        }
        report
    }

    /// Whether an ESC that ended a read waits for a timeout: start one, and
    /// call `flush_pending` when it fires without more input. (See
    /// `Parser::flush_due`.)
    pub(crate) fn escape_pending(&self) -> bool {
        self.parser.flush_due()
    }

    /// When the ESC waiting for a timeout is due to be the Escape key: the
    /// timeout after the read that left it. None if none waits. Each
    /// forwarder has its own, so one pane's reads don't put off another's.
    pub(crate) fn escape_deadline(&self) -> Option<Instant> {
        self.escape_since
            .map(|t| t + Duration::from_millis(u64::from(input::ESCAPE_TIMEOUT_MS)))
    }

    /// Notes, after a read, whether an ESC now waits and since when.
    fn note_escape(&mut self) {
        self.escape_since = if self.parser.flush_due() {
            Some(Instant::now())
        } else {
            None
        };
    }

    /// The timeout for a pending ESC: nothing followed it, so it is the Escape
    /// key. Sends it. Does nothing if no ESC is waiting.
    pub(crate) fn flush_pending(&mut self, nvim: &mut Nvim) -> Result<()> {
        let flushed = self.parser.flush_pending();
        self.escape_since = None;
        let Some(Event::Key(k)) = flushed else {
            return Ok(());
        };
        let mut keys = Vec::new();
        input::write_nvim(k, &mut keys);
        send_keys(nvim, &mut keys)?;
        nvim.flush()?;
        Ok(())
    }

    /// Whether the parser is between inputs, not partway through an escape
    /// sequence or character.
    pub(crate) fn at_boundary(&self) -> bool {
        self.parser.at_ground()
    }

    fn read(&mut self, nvim: &mut Nvim, bytes: &[u8], one_key: bool) -> Result<Fed> {
        // keys from one read go in one nvim_input
        let mut keys: Vec<u8> = Vec::with_capacity(KEYS_CAPACITY);
        let mut result = Fed {
            more: true,
            read: bytes.len(),
            stopped_after_key: false,
            sent_input: false,
        };
        self.parser.feed(bytes);
        while let Some(event) = self.parser.next() {
            match event {
                Event::Key(k) => {
                    if !one_key && self.ctrl_q_quits && is_quit(k) {
                        return Ok(Fed {
                            more: false,
                            read: bytes.len(),
                            stopped_after_key: false,
                            sent_input: result.sent_input,
                        });
                    }
                    // room for the longest key name: flush before the buffer
                    // can't hold it
                    if KEYS_CAPACITY - keys.len() < 32 {
                        result.sent_input |= send_keys(nvim, &mut keys)?;
                    }
                    input::write_nvim(k, &mut keys);
                    if one_key {
                        result = Fed {
                            more: true,
                            read: bytes.len() - self.parser.take_rest().len(),
                            stopped_after_key: true,
                            sent_input: result.sent_input,
                        };
                        break;
                    }
                }
                Event::PasteStart => self.start_paste(),
                Event::Paste(text) => {
                    // the LF of a CRLF cut between chunks
                    let text = match text {
                        [b'\n', rest @ ..] if self.paste_ended_in_cr => {
                            self.paste_ended_in_cr = false;
                            rest
                        }
                        _ => text,
                    };
                    if let Some(&last) = text.last() {
                        self.paste_ended_in_cr = last == b'\r';
                    }
                    if !text.is_empty() {
                        result.sent_input |= send_keys(nvim, &mut keys)?;
                        paste(nvim, text, self.paste_phase)?;
                        self.paste_phase = 2;
                    }
                }
                Event::PasteEnd if self.paste_phase == 2 => paste(nvim, b"", 3)?,
                Event::PasteEnd => {}
                Event::Focus(focused) => {
                    self.focused = Some(focused);
                    if self.send_focus {
                        nvim.request("nvim_ui_set_focus", 1);
                        msgpack::write_bool(nvim.writer(), focused);
                    }
                }
                Event::Mouse(m) => {
                    if let Some(mut args) = MouseArgs::from(m) {
                        // keys typed before the click go first
                        result.sent_input |= send_keys(nvim, &mut keys)?;
                        if let Some(to) = self.route(m) {
                            args.row = to.row;
                            args.col = to.col;
                            args.send(nvim, to.grid);
                        }
                    }
                }
                Event::Unknown(_) => {}
            }
        }
        self.note_escape();
        result.sent_input |= send_keys(nvim, &mut keys)?;
        nvim.flush()?;
        Ok(result)
    }
}

/// Sends buffered keys with `nvim_input`, then empties the buffer.
fn send_keys(nvim: &mut Nvim, keys: &mut Vec<u8>) -> Result<bool> {
    if keys.is_empty() {
        return Ok(false);
    }
    nvim.request("nvim_input", 1);
    msgpack::write_str(nvim.writer(), &keys[..]);
    keys.clear();
    Ok(true)
}

/// One piece of a streamed `nvim_paste`. Terminals send newlines as CR, and
/// `crlf` makes Neovim break lines at CR, LF and CRLF alike.
fn paste(nvim: &mut Nvim, text: &[u8], phase: i64) -> Result<()> {
    nvim.request("nvim_paste", 3);
    let len = u32::try_from(text.len()).map_err(|_| Error::TooLong)?;
    let w = nvim.writer();
    msgpack::write_str_header(w, len);
    w.extend_from_slice(text);
    msgpack::write_bool(w, true); // crlf
    msgpack::write_int(w, phase);
    Ok(())
}

/// `nvim_input_mouse`'s arguments for a terminal mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseArgs {
    button: &'static str,
    action: &'static str,
    mods: [u8; 8],
    mods_len: u8,
    row: u16,
    col: u16,
}

impl MouseArgs {
    /// None for events Neovim has no name for.
    fn from(m: input::Mouse) -> Option<MouseArgs> {
        let (button, action): (&'static str, &'static str) = match m.button {
            MouseButton::Left | MouseButton::Middle | MouseButton::Right => (
                m.button.tag_name(),
                match m.action {
                    MouseAction::Press => "press",
                    MouseAction::Drag => "drag",
                    MouseAction::Release => "release",
                    MouseAction::Motion => return None,
                },
            ),
            MouseButton::WheelUp => ("wheel", "up"),
            MouseButton::WheelDown => ("wheel", "down"),
            MouseButton::WheelLeft => ("wheel", "left"),
            MouseButton::WheelRight => ("wheel", "right"),
            MouseButton::None => {
                if m.action == MouseAction::Motion {
                    ("move", "")
                } else {
                    return None;
                }
            }
        };
        let mut a = MouseArgs {
            button,
            action,
            mods: [0; 8],
            mods_len: 0,
            row: m.row,
            col: m.col,
        };
        // as in key names: C-A-S-D-
        for (on, letter) in [
            (m.mods.ctrl, b'C'),
            (m.mods.alt, b'A'),
            (m.mods.shift, b'S'),
            (m.mods.super_, b'D'),
        ] {
            if !on {
                continue;
            }
            a.mods[a.mods_len as usize] = letter;
            a.mods[a.mods_len as usize + 1] = b'-';
            a.mods_len += 2;
        }
        Some(a)
    }

    fn send(&self, nvim: &mut Nvim, grid: u32) {
        nvim.request("nvim_input_mouse", 6);
        let w = nvim.writer();
        msgpack::write_str(w, self.button);
        msgpack::write_str(w, self.action);
        msgpack::write_str(w, &self.mods[..self.mods_len as usize]);
        msgpack::write_int(w, grid);
        msgpack::write_int(w, self.row);
        msgpack::write_int(w, self.col);
    }
}

fn is_quit(k: input::KeyEvent) -> bool {
    k.mods.ctrl && !k.mods.alt && k.key == input::Key::Codepoint('q' as u32)
}

/// Whether `k` is Ctrl-C, which interrupts what Neovim is doing.
pub(crate) fn is_interrupt(k: input::KeyEvent) -> bool {
    k.mods.ctrl && !k.mods.alt && !k.mods.shift && k.key == input::Key::Codepoint('c' as u32)
}

/// Sends Ctrl-C to Neovim and flushes, without the parser: for one read
/// whole that held nothing else.
pub(crate) fn interrupt(nvim: &mut Nvim) -> Result<()> {
    let mut keys = Vec::new();
    let ctrl = input::Mods {
        ctrl: true,
        ..input::Mods::default()
    };
    input::write_nvim(
        input::KeyEvent {
            key: input::Key::Codepoint('c' as u32),
            mods: ctrl,
        },
        &mut keys,
    );
    send_keys(nvim, &mut keys)?;
    nvim.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::input::{Mods, Mouse};

    fn mouse(button: MouseButton, action: MouseAction, row: u16, col: u16) -> Mouse {
        Mouse {
            button,
            action,
            mods: Mods::default(),
            row,
            col,
        }
    }

    #[test]
    fn mouse_events_become_nvim_input_mouse_args() {
        let drag = MouseArgs::from(Mouse {
            mods: Mods {
                ctrl: true,
                shift: true,
                ..Mods::default()
            },
            ..mouse(MouseButton::Left, MouseAction::Drag, 3, 7)
        })
        .unwrap();
        assert_eq!("left", drag.button);
        assert_eq!("drag", drag.action);
        assert_eq!(b"C-S-", &drag.mods[..drag.mods_len as usize]);
        assert_eq!(3, drag.row);

        let wheel =
            MouseArgs::from(mouse(MouseButton::WheelDown, MouseAction::Press, 0, 0)).unwrap();
        assert_eq!("wheel", wheel.button);
        assert_eq!("down", wheel.action);
        assert_eq!(b"", &wheel.mods[..wheel.mods_len as usize]);

        let mv = MouseArgs::from(mouse(MouseButton::None, MouseAction::Motion, 1, 1)).unwrap();
        assert_eq!("move", mv.button);
        assert_eq!(
            None,
            MouseArgs::from(mouse(MouseButton::None, MouseAction::Release, 0, 0))
        );
    }

    #[test]
    fn mouse_events_on_a_float_go_to_its_grid() {
        let mut f = Forwarder {
            grid: 2,
            ..Forwarder::default()
        };
        f.set_targets(&[
            Target {
                grid: 3,
                row: 1,
                col: 1,
                width: 10,
                height: 5,
                ..Target::default()
            },
            // above the first
            Target {
                grid: 4,
                row: 2,
                col: 2,
                width: 3,
                height: 1,
                ..Target::default()
            },
        ]);
        let off = f
            .route(mouse(MouseButton::Left, MouseAction::Press, 0, 0))
            .unwrap();
        assert_eq!(2, off.grid);
        assert_eq!(0, off.row);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 0));

        let top = f
            .route(mouse(MouseButton::Left, MouseAction::Press, 2, 3))
            .unwrap();
        assert_eq!(4, top.grid);
        assert_eq!(0, top.row);
        assert_eq!(1, top.col);
        // dragged off it: still that grid, clamped
        let drag = f
            .route(mouse(MouseButton::Left, MouseAction::Drag, 0, 9))
            .unwrap();
        assert_eq!(4, drag.grid);
        assert_eq!(0, drag.row);
        assert_eq!(7, drag.col);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 9));
        assert_eq!(None, f.grab);

        let under = f
            .route(mouse(MouseButton::WheelDown, MouseAction::Press, 4, 1))
            .unwrap();
        assert_eq!(3, under.grid);
        assert_eq!(3, under.row);
    }

    #[test]
    fn mouse_events_in_a_pane_layout_go_to_the_window_under_them_and_off_them_nowhere() {
        // two windows side by side under the tabline, a separator at column
        // 5, and the messages below: the left one's last row a statusline of
        // neorex's; the right one cut to 4 rows, its last its statusline,
        // its grid's row 7
        let mut f = Forwarder {
            grid: 2,
            tabline_rows: 1,
            ..Forwarder::default()
        };
        let left = Target {
            grid: 2,
            width: 5,
            height: 3,
            ..Target::default()
        };
        let right = Target {
            grid: 3,
            col: 6,
            width: 4,
            height: 3,
            ..Target::default()
        };
        let right_statusline = Target {
            row: 3,
            height: 1,
            grid_row: 7,
            ..right
        };
        f.set_windows([left, right, right_statusline]);
        f.set_targets(&[Target {
            grid: 9,
            row: 3,
            col: 7,
            width: 2,
            height: 1,
            ..Target::default()
        }]);
        let press = |f: &mut Forwarder, row, col| {
            let at = f.route(mouse(MouseButton::Left, MouseAction::Press, row, col));
            f.route(mouse(MouseButton::Left, MouseAction::Release, row, col));
            at.map(|r| (r.grid, r.row, r.col))
        };
        assert_eq!(Some((3, 1, 2)), press(&mut f, 2, 8));
        assert_eq!(Some((2, 0, 4)), press(&mut f, 1, 4));
        // the separator, the statusline neorex draws: no grid's
        assert_eq!(None, press(&mut f, 2, 5));
        assert_eq!(None, press(&mut f, 4, 1));
        assert_eq!(None, f.grab);
        // the statusline of the window cut short: its grid's last row
        assert_eq!(Some((3, 7, 3)), press(&mut f, 4, 9));
        // the messages, below them all: the current window's grid, as with one
        assert_eq!(Some((2, 4, 3)), press(&mut f, 5, 3));
        // a float over a window first, the tabline as ever
        assert_eq!(Some((9, 0, 1)), press(&mut f, 4, 8));
        assert_eq!(Some((1, 0, 5)), press(&mut f, 0, 5));
        // dragged across the separator: still the window pressed
        f.route(mouse(MouseButton::Left, MouseAction::Press, 1, 2));
        let drag = f.route(mouse(MouseButton::Left, MouseAction::Drag, 1, 7));
        assert_eq!(Some((2, 0, 7)), drag.map(|r| (r.grid, r.row, r.col)));
        f.route(mouse(MouseButton::Left, MouseAction::Release, 1, 7));
        assert_eq!(None, f.grab);
        // one window again: its grid takes them all
        f.set_windows([]);
        assert_eq!(Some((2, 1, 5)), press(&mut f, 2, 5));
    }

    #[test]
    fn mouse_events_on_the_tabline_go_to_grid_1_and_below_it_move_up() {
        let mut f = Forwarder {
            grid: 2,
            tabline_rows: 1,
            tabline_col: 40,
            ..Forwarder::default()
        };
        f.set_targets(&[Target {
            grid: 3,
            row: 1,
            col: 0,
            width: 5,
            height: 1,
            ..Target::default()
        }]);
        let tab = f
            .route(mouse(MouseButton::Left, MouseAction::Press, 0, 3))
            .unwrap();
        assert_eq!(1, tab.grid);
        assert_eq!(0, tab.row);
        assert_eq!(43, tab.col);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 3));
        // the window's first row is under the tabline
        let win = f
            .route(mouse(MouseButton::Left, MouseAction::Press, 1, 7))
            .unwrap();
        assert_eq!(2, win.grid);
        assert_eq!(0, win.row);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 1, 7));
        // floats are placed on the window's grid too
        let float = f
            .route(mouse(MouseButton::WheelDown, MouseAction::Press, 2, 1))
            .unwrap();
        assert_eq!(3, float.grid);
    }

    #[test]
    fn tabline_clicks_in_panes_that_continue_the_tabline_select_its_next_columns() {
        // two panes of 60 columns: the second shows tabline columns 60 to 119
        let mut left = Forwarder {
            grid: 2,
            tabline_rows: 1,
            tabline_col: 0,
            ..Forwarder::default()
        };
        let mut right = Forwarder {
            grid: 4,
            tabline_rows: 1,
            tabline_col: 60,
            ..Forwarder::default()
        };
        let a = left
            .route(mouse(MouseButton::Left, MouseAction::Press, 0, 59))
            .unwrap();
        let b = right
            .route(mouse(MouseButton::Left, MouseAction::Press, 0, 0))
            .unwrap();
        assert_eq!(1, a.grid);
        assert_eq!(1, b.grid);
        assert_eq!(59, a.col);
        assert_eq!(60, b.col);
    }

    #[test]
    fn only_keys_pastes_and_button_presses_take_focus() {
        let f = Forwarder::default();
        assert!(f.takes_focus(b"x"));
        assert!(f.takes_focus(b"\x1b[A"));
        assert!(f.takes_focus(b"\x1b"));
        assert!(f.takes_focus(b"\x1b[200~hi\x1b[201~"));
        assert!(f.takes_focus(b"\x1b[<0;5;5M"));
        assert!(f.takes_focus(b"\x1b[<2;5;5M"));
        // a click among reports and scrolling
        assert!(f.takes_focus(b"\x1b[I\x1b[<65;5;5M\x1b[<0;5;5M"));
        assert!(f.takes_focus(b"\x1b[<65;5;5Mx"));

        assert!(!f.takes_focus(b""));
        assert!(!f.takes_focus(b"\x1b[I"));
        assert!(!f.takes_focus(b"\x1b[O\x1b[I"));
        assert!(!f.takes_focus(b"\x1b[<64;5;5M"));
        assert!(!f.takes_focus(b"\x1b[<65;5;5M\x1b[<65;5;5M"));
        assert!(!f.takes_focus(b"\x1b[<66;5;5M\x1b[<67;5;5M"));
        // the pointer moving, a button released or dragged
        assert!(!f.takes_focus(b"\x1b[<35;5;5M"));
        assert!(!f.takes_focus(b"\x1b[<0;5;5m"));
        assert!(!f.takes_focus(b"\x1b[<32;5;5M"));
    }

    #[test]
    fn the_last_focus_report_in_a_read_is_the_one_that_stands() {
        let f = Forwarder::default();
        assert_eq!(None, f.focus_report(b"x"));
        assert_eq!(Some(FocusReport::In), f.focus_report(b"\x1b[I"));
        assert_eq!(Some(FocusReport::Out), f.focus_report(b"\x1b[Ix\x1b[O"));
    }

    #[test]
    fn a_click_among_keys_takes_focus_as_a_click() {
        let f = Forwarder::default();
        assert_eq!(Taking::Key, f.taking(b"x\x1b[I"));
        assert_eq!(Taking::Key, f.taking(b"\x1b[200~hi\x1b[201~"));
        assert_eq!(Taking::Click, f.taking(b"x\x1b[<0;5;5M"));
        assert_eq!(Taking::Nothing, f.taking(b"\x1b[<65;5;5M"));
    }

    #[test]
    fn asking_whether_input_takes_focus_reads_from_where_the_parser_is_and_leaves_it_there() {
        let mut f = Forwarder::default();
        // a wheel report cut in two reads
        f.parser.feed(b"\x1b[<");
        while f.parser.next().is_some() {}
        assert!(!f.at_boundary());
        assert!(!f.takes_focus(b"65;5;5M"));
        assert!(f.takes_focus(b"0;5;5M"));
        assert!(!f.at_boundary());
    }

    /// The current buffer's lines.
    fn buffer_lines(nvim: &mut Nvim) -> Vec<Vec<u8>> {
        use crate::nvim::nvim::wait_response;
        use crate::nvim::rpc;
        let id = nvim.request("nvim_buf_get_lines", 4);
        let w = nvim.writer();
        msgpack::write_int(w, 0);
        msgpack::write_int(w, 0);
        msgpack::write_int(w, -1);
        msgpack::write_bool(w, false);
        nvim.flush().unwrap();
        let frame = wait_response(nvim, id);
        let rpc::Message::Response { mut result, .. } = frame.message().unwrap() else {
            panic!("not a response");
        };
        let n = rpc::expect_array(&mut result).unwrap();
        (0..n)
            .map(|_| rpc::expect_str(&mut result).unwrap().to_vec())
            .collect()
    }

    /// Pastes each of `chunks` as its own read of one bracketed paste into an
    /// empty buffer of a live Neovim, and returns the buffer's lines.
    fn paste_chunks(chunks: &[&[u8]]) -> Option<Vec<Vec<u8>>> {
        let mut nvim = crate::nvim::nvim::spawn_for_test()?;
        nvim.ui_attach(80, 24, Default::default()).unwrap();
        let mut f = Forwarder::default();
        f.feed(&mut nvim, b"\x1b[200~").unwrap();
        for chunk in chunks {
            f.feed(&mut nvim, chunk).unwrap();
        }
        f.feed(&mut nvim, b"\x1b[201~").unwrap();
        Some(buffer_lines(&mut nvim))
    }

    #[test]
    fn pasted_line_breaks_each_give_one_line_in_neovim() {
        let ab = vec![b"a".to_vec(), b"b".to_vec()];
        for text in [&b"a\r\nb"[..], b"a\rb", b"a\nb"] {
            let Some(got) = paste_chunks(&[text]) else {
                return;
            };
            assert_eq!(ab, got, "{:?}", String::from_utf8_lossy(text));
        }
        // a CRLF cut between two chunks
        assert_eq!(Some(ab), paste_chunks(&[b"a\r", b"\nb"]));
        // the LF alone in a chunk finishes that CRLF; the next LF is a break
        assert_eq!(
            Some(vec![b"a".to_vec(), b"".to_vec(), b"b".to_vec()]),
            paste_chunks(&[b"a\r", b"\n", b"\nb"])
        );
    }

    /// Feeds a live Neovim keys and a paste.
    #[test]
    fn keys_and_pastes_reach_neovim() {
        use crate::nvim::nvim::spawn_for_test;
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        // an embedded Neovim starts up once a UI attaches
        nvim.ui_attach(80, 24, Default::default()).unwrap();
        let mut f = Forwarder::default();
        let fed = f.feed(&mut nvim, b"ihi").unwrap();
        assert!(fed.more && fed.sent_input);
        // nvim_input is queued; wait for Neovim to type it
        let mut got = buffer_lines(&mut nvim);
        for _ in 0..200 {
            if got == [b"hi".to_vec()] {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            got = buffer_lines(&mut nvim);
        }
        assert_eq!(vec![b"hi".to_vec()], got);
        assert!(f.feed(&mut nvim, b"\x1b[200~a\rb\x1b[201~").unwrap().more);
        assert_eq!(
            vec![b"hia".to_vec(), b"b".to_vec()],
            buffer_lines(&mut nvim)
        );
        // Ctrl+Q quits
        assert!(!f.feed(&mut nvim, b"\x11").unwrap().more);
    }

    #[test]
    fn each_forwarders_waiting_esc_has_its_own_deadline() {
        let Some(mut nvim) = crate::nvim::nvim::spawn_for_test() else {
            return;
        };
        let mut a = Forwarder::default();
        let mut b = Forwarder::default();
        assert_eq!(None, a.escape_deadline());
        // no key yet: the ESC waits
        assert!(!a.feed(&mut nvim, b"\x1b").unwrap().sent_input);
        let due = a.escape_deadline().expect("A's ESC waits");
        let timeout = Duration::from_millis(u64::from(input::ESCAPE_TIMEOUT_MS));
        assert!(due <= Instant::now() + timeout);
        // B keeps getting reads: A's deadline stays
        std::thread::sleep(Duration::from_millis(5));
        for _ in 0..3 {
            b.feed(&mut nvim, b"\x1b[<35;3;3M").unwrap();
            assert!(!b.feed(&mut nvim, b"\x1b[I").unwrap().sent_input);
        }
        assert_eq!(Some(due), a.escape_deadline());
        assert_eq!(None, b.escape_deadline());
        // the timeout: sent, and nothing waits
        a.flush_pending(&mut nvim).unwrap();
        assert_eq!(None, a.escape_deadline());
        // a read that leaves an ESC waiting sets a new deadline
        a.feed(&mut nvim, b"x\x1b").unwrap();
        assert!(a.escape_deadline().unwrap() > due);
        // and one that completes it clears it
        a.feed(&mut nvim, b"[A").unwrap();
        assert_eq!(None, a.escape_deadline());
    }
}
