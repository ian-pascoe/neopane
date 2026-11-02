//! Turns terminal input into Neovim API calls: keys, pastes, focus and mouse.

use std::io;

use super::msgpack;
use super::nvim::Nvim;
use crate::term::input::{self, Event, MouseAction, MouseButton};

/// What can go wrong forwarding input: Neovim's pipe failing, or a paste too
/// long for a msgpack string.
#[derive(Debug)]
pub enum Error {
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

pub type Result<T> = std::result::Result<T, Error>;

/// Room for the keys of one read.
const KEYS_CAPACITY: usize = 4096;

pub struct Forwarder {
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
    /// The float a button went down on: its drags and release go there too.
    pub grab: Option<Target>,
    /// Off when the caller tells Neovim about focus itself, from `focused`.
    pub send_focus: bool,
    /// The last focus report seen.
    pub focused: Option<bool>,
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
            grab: None,
            send_focus: true,
            focused: None,
        }
    }
}

/// A float's grid and where it's drawn, in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Target {
    pub grid: u32,
    pub row: i32,
    pub col: i32,
    pub width: u32,
    pub height: u32,
}

pub const MAX_TARGETS: usize = 16;

/// The grid a mouse event lands on, and where on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    grid: u32,
    row: u16,
    col: u16,
}

/// What `feed` and `feed_key` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fed {
    /// False if the user quit.
    pub more: bool,
    /// How many bytes were read: all, unless `feed_key` stopped after a key.
    pub read: usize,
    pub sent_key: bool,
}

impl Forwarder {
    pub fn set_targets(&mut self, targets: &[Target]) {
        let n = targets.len().min(MAX_TARGETS);
        self.targets[..n].copy_from_slice(&targets[..n]);
        self.targets_len = n as u8;
    }

    /// The grid a mouse event lands on, and where on it.
    fn route(&mut self, event: input::Mouse) -> Route {
        let held = event.action == MouseAction::Drag || event.action == MouseAction::Release;
        if event.row < self.tabline_rows && !(held && self.grab.is_some()) {
            if event.action == MouseAction::Release {
                self.grab = None;
            }
            return Route {
                grid: 1,
                row: event.row,
                col: event.col.saturating_add(self.tabline_col),
            };
        }
        let mut m = event;
        m.row = m.row.saturating_sub(self.tabline_rows);
        let hit: Option<Target> = if held && self.grab.is_some() {
            self.grab
        } else {
            let (row, col) = (m.row as i64, m.col as i64);
            (0..self.targets_len as usize)
                .map(|j| self.targets[self.targets_len as usize - 1 - j])
                .find(|t| {
                    row >= t.row as i64
                        && row < t.row as i64 + t.height as i64
                        && col >= t.col as i64
                        && col < t.col as i64 + t.width as i64
                })
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
            return Route {
                grid: self.grid,
                row: m.row,
                col: m.col,
            };
        };
        Route {
            grid: t.grid,
            row: (m.row as i64 - t.row as i64).clamp(0, u16::MAX as i64) as u16,
            col: (m.col as i64 - t.col as i64).clamp(0, u16::MAX as i64) as u16,
        }
    }

    /// Sends the input in `bytes` to Neovim and flushes. False if the user
    /// pressed Ctrl+Q to quit and `ctrl_q_quits` is set.
    pub fn feed(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<bool> {
        Ok(self.read(nvim, bytes, false)?.more)
    }

    /// Begins a paste: the next chunk starts it.
    fn start_paste(&mut self) {
        self.paste_phase = 1;
        self.paste_ended_in_cr = false;
    }

    /// Like `feed`, but stops after the first key: what follows it is left for
    /// a later `feed`. Doesn't quit.
    pub fn feed_key(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<Fed> {
        self.read(nvim, bytes, true)
    }

    /// Reads `bytes` as `feed` does, but sends nothing: for input whose window
    /// is gone. Keeps what the forwarder remembers of the input, such as focus,
    /// and closes a paste that was begun.
    pub fn discard(&mut self, nvim: &mut Nvim, bytes: &[u8]) -> Result<()> {
        self.parser.feed(bytes);
        while let Some(event) = self.parser.next() {
            match event {
                Event::Focus(focused) => self.focused = Some(focused),
                Event::PasteStart => self.start_paste(),
                Event::PasteEnd if self.paste_phase == 2 => paste(nvim, b"", 3)?,
                _ => {}
            }
        }
        nvim.flush()?;
        Ok(())
    }

    /// Whether `bytes`, read as `feed` reads them, are typed or clicked into
    /// the pane, as opposed to what only reports or looks: focus reports, and
    /// mouse events other than a button going down, such as the wheel and a
    /// pointer moving. Changes nothing: the parser is read from a copy.
    pub fn takes_focus(&self, bytes: &[u8]) -> bool {
        let mut parser = self.parser.clone();
        parser.feed(bytes);
        while let Some(event) = parser.next() {
            match event {
                Event::Focus(_) => {}
                Event::Mouse(m) => match m.action {
                    MouseAction::Press => match m.button {
                        MouseButton::WheelUp
                        | MouseButton::WheelDown
                        | MouseButton::WheelLeft
                        | MouseButton::WheelRight => {}
                        _ => return true,
                    },
                    MouseAction::Release | MouseAction::Drag | MouseAction::Motion => {}
                },
                Event::Key(_)
                | Event::PasteStart
                | Event::Paste(_)
                | Event::PasteEnd
                | Event::Unknown(_) => return true,
            }
        }
        false
    }

    /// Whether the parser is between inputs, not partway through an escape
    /// sequence or character.
    pub fn at_boundary(&self) -> bool {
        self.parser.at_ground()
    }

    fn read(&mut self, nvim: &mut Nvim, bytes: &[u8], one_key: bool) -> Result<Fed> {
        // keys from one read go in one nvim_input
        let mut keys: Vec<u8> = Vec::with_capacity(KEYS_CAPACITY);
        let mut result = Fed {
            more: true,
            read: bytes.len(),
            sent_key: false,
        };
        self.parser.feed(bytes);
        while let Some(event) = self.parser.next() {
            match event {
                Event::Key(k) => {
                    if !one_key && self.ctrl_q_quits && is_quit(k) {
                        return Ok(Fed {
                            more: false,
                            read: bytes.len(),
                            sent_key: false,
                        });
                    }
                    // room for the longest key name: flush before the buffer
                    // can't hold it
                    if KEYS_CAPACITY - keys.len() < 32 {
                        send_keys(nvim, &mut keys)?;
                    }
                    input::write_nvim(k, &mut keys);
                    if one_key {
                        result = Fed {
                            more: true,
                            read: bytes.len() - self.parser.take_rest().len(),
                            sent_key: true,
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
                        send_keys(nvim, &mut keys)?;
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
                        send_keys(nvim, &mut keys)?;
                        let to = self.route(m);
                        args.row = to.row;
                        args.col = to.col;
                        args.send(nvim, to.grid);
                    }
                }
                Event::Unknown(_) => {}
            }
        }
        send_keys(nvim, &mut keys)?;
        nvim.flush()?;
        Ok(result)
    }
}

/// Sends buffered keys with `nvim_input`, then empties the buffer.
fn send_keys(nvim: &mut Nvim, keys: &mut Vec<u8>) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    nvim.request("nvim_input", 1);
    msgpack::write_str(nvim.writer(), &keys[..]);
    keys.clear();
    Ok(())
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
            },
            // above the first
            Target {
                grid: 4,
                row: 2,
                col: 2,
                width: 3,
                height: 1,
            },
        ]);
        let off = f.route(mouse(MouseButton::Left, MouseAction::Press, 0, 0));
        assert_eq!(2, off.grid);
        assert_eq!(0, off.row);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 0));

        let top = f.route(mouse(MouseButton::Left, MouseAction::Press, 2, 3));
        assert_eq!(4, top.grid);
        assert_eq!(0, top.row);
        assert_eq!(1, top.col);
        // dragged off it: still that grid, clamped
        let drag = f.route(mouse(MouseButton::Left, MouseAction::Drag, 0, 9));
        assert_eq!(4, drag.grid);
        assert_eq!(0, drag.row);
        assert_eq!(7, drag.col);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 9));
        assert_eq!(None, f.grab);

        let under = f.route(mouse(MouseButton::WheelDown, MouseAction::Press, 4, 1));
        assert_eq!(3, under.grid);
        assert_eq!(3, under.row);
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
        }]);
        let tab = f.route(mouse(MouseButton::Left, MouseAction::Press, 0, 3));
        assert_eq!(1, tab.grid);
        assert_eq!(0, tab.row);
        assert_eq!(43, tab.col);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 0, 3));
        // the window's first row is under the tabline
        let win = f.route(mouse(MouseButton::Left, MouseAction::Press, 1, 7));
        assert_eq!(2, win.grid);
        assert_eq!(0, win.row);
        f.route(mouse(MouseButton::Left, MouseAction::Release, 1, 7));
        // floats are placed on the window's grid too
        let float = f.route(mouse(MouseButton::WheelDown, MouseAction::Press, 2, 1));
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
        let a = left.route(mouse(MouseButton::Left, MouseAction::Press, 0, 59));
        let b = right.route(mouse(MouseButton::Left, MouseAction::Press, 0, 0));
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
        assert!(f.feed(&mut nvim, b"ihi").unwrap());
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
        assert!(f.feed(&mut nvim, b"\x1b[200~a\rb\x1b[201~").unwrap());
        assert_eq!(
            vec![b"hia".to_vec(), b"b".to_vec()],
            buffer_lines(&mut nvim)
        );
        // Ctrl+Q quits
        assert!(!f.feed(&mut nvim, b"\x11").unwrap());
    }
}
