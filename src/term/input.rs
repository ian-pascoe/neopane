//! AI Generated, needs to be done properly
//! Terminal input: turns the bytes a terminal sends into events.
//!
//! Keys arrive in a few shapes:
//!
//!   a, é, 漢         the character itself (UTF-8, 1 to 4 bytes)
//!   Enter, Tab       single control bytes: 0x0d, 0x09
//!   Ctrl+A .. Ctrl+Z control bytes 0x01 .. 0x1a
//!   Escape           0x1b on its own
//!   Alt+x            0x1b then x
//!   Up, Ctrl+Right   CSI sequences: ESC [ params final, e.g. ESC [ 1 ; 5 C
//!   Delete, F5       CSI ... ~ : ESC [ 3 ~, ESC [ 1 5 ~
//!   F1 (some terms)  SS3 sequences: ESC O P
//!
//! That "legacy" encoding loses information: Ctrl+I and Tab are the same byte,
//! and a lone ESC could be the Escape key or the start of a sequence. The
//! kitty keyboard protocol fixes this. Write `KITTY_PUSH` to turn it on and
//! `KITTY_POP` to turn it off. While it's on, any key the legacy encoding
//! would garble arrives as a "CSI u" sequence instead:
//!
//!   ESC [ code : shifted ; mods : event u
//!
//!   Escape           ESC [ 27 u
//!   Ctrl+I           ESC [ 105 ; 5 u        (Tab is still 0x09)
//!   Alt+Shift+1      ESC [ 49 : 33 ; 4 u    (33 is '!')
//!
//! Plain typing, arrows and F-keys keep their legacy forms, so the parser
//! handles both. Spec: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
//!
//! The parser is a small state machine fed one byte at a time, in the style of
//! Ghostty's VT parser (which follows vt100.net/emu/dec_ansi_parser). Its state
//! survives between reads, so a sequence split across two reads just carries
//! on where it left off.
//!
//! Usage:
//!
//! ```text
//! let mut parser = Parser::default();
//! parser.feed(bytes_from_read);
//! while let Some(event) = parser.next() { ... }
//! ```
//!
//! Rust port note: `Event::Paste` and `Event::Unknown` borrow from the parser
//! (the borrow checker enforces "only valid until the next `next` or `feed`"),
//! so `feed` copies the bytes into the parser's own buffer, and `take_rest`
//! returns an owned `Vec`.

use std::fmt;

/// `KITTY_PUSH` as a literal, so `concat!` can use it (it can't take consts).
macro_rules! kitty_push_literal {
    () => {
        "\x1b[>5u"
    };
}
/// `KITTY_POP` as a literal, so `concat!` can use it.
macro_rules! kitty_pop_literal {
    () => {
        "\x1b[<u"
    };
}
pub(crate) use kitty_pop_literal;
pub(crate) use kitty_push_literal;

/// Turns the kitty keyboard protocol on, with flags 1 (disambiguate: use CSI u
/// for keys the legacy encoding garbles) and 4 (report the shifted key, so
/// Alt+Shift+1 can be `<M-!>`). The terminal keeps a stack of flags, one per
/// screen (main and alternate), so push after switching screens.
pub const KITTY_PUSH: &str = kitty_push_literal!();
/// Turns it back off: pops what `KITTY_PUSH` pushed.
pub const KITTY_POP: &str = kitty_pop_literal!();

/// Modifier keys held with a key or mouse event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_: bool,
}

impl Mods {
    /// From the modifier parameter in sequences like `ESC [ 1 ; 5 C`. The
    /// value minus one is a set of bits: 1 shift, 2 alt, 4 ctrl, 8 super.
    /// Higher bits (hyper, meta, caps lock, num lock) are ignored.
    pub fn from_param(param: u32) -> Mods {
        if param <= 1 {
            return Mods::default();
        }
        let bits = param - 1;
        Mods {
            shift: bits & 1 != 0,
            alt: bits & 2 != 0,
            ctrl: bits & 4 != 0,
            super_: bits & 8 != 0,
        }
    }

    pub fn is_empty(self) -> bool {
        !self.shift && !self.alt && !self.ctrl && !self.super_
    }
}

/// Keys that aren't characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Which key: a character, or a named key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character, as a Unicode codepoint (Zig's `u21`; a `u32` here, since a
    /// terminal can send a surrogate, which `char` can't hold). Ctrl+A is
    /// `'a'` with `ctrl` set.
    Codepoint(u32),
    Named(Named),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    /// A key with no modifiers (Zig's default for `mods`).
    pub fn new(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            mods: Mods::default(),
        }
    }
}

/// `Mouse.Button` in the Zig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    /// The name Zig's `@tagName` gives.
    pub fn tag_name(self) -> &'static str {
        match self {
            MouseButton::Left => "left",
            MouseButton::Middle => "middle",
            MouseButton::Right => "right",
            MouseButton::None => "none",
            MouseButton::WheelUp => "wheel_up",
            MouseButton::WheelDown => "wheel_down",
            MouseButton::WheelLeft => "wheel_left",
            MouseButton::WheelRight => "wheel_right",
        }
    }
}

/// `Mouse.Action` in the Zig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Drag,
    Motion,
}

impl MouseAction {
    /// The name Zig's `@tagName` gives.
    pub fn tag_name(self) -> &'static str {
        match self {
            MouseAction::Press => "press",
            MouseAction::Release => "release",
            MouseAction::Drag => "drag",
            MouseAction::Motion => "motion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub button: MouseButton,
    pub action: MouseAction,
    pub mods: Mods,
    /// The cell under the pointer, counting from 0.
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    Key(KeyEvent),
    /// Bracketed paste: `PasteStart`, then the text in one or more `Paste`
    /// chunks, then `PasteEnd`. Pasted text never turns into key events.
    PasteStart,
    /// Pasted text. Points into the bytes given to `feed` (or the parser's
    /// own memory), so it is only valid until the next `next` or `feed`.
    Paste(&'a [u8]),
    PasteEnd,
    /// The terminal gained (`true`) or lost (`false`) focus.
    Focus(bool),
    Mouse(Mouse),
    /// A sequence the parser doesn't know, as raw bytes. Valid until the next
    /// `next` or `feed`.
    Unknown(&'a [u8]),
}

/// Zig's `std.ascii.hexEscape(bytes, .lower)`: printable ASCII as is, the
/// rest as `\xNN`.
fn write_hex_escape(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for &c in bytes {
        if (0x20..=0x7e).contains(&c) {
            write!(f, "{}", c as char)?;
        } else {
            write!(f, "\\x{c:02x}")?;
        }
    }
    Ok(())
}

/// For printing with `{}`, e.g. in `debug-keys`.
impl fmt::Display for Event<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Key(k) => {
                f.write_str("key ")?;
                let mut buf = Vec::new();
                write_nvim(*k, &mut buf);
                f.write_str(&String::from_utf8_lossy(&buf))
            }
            Event::PasteStart => f.write_str("paste start"),
            Event::Paste(text) => {
                f.write_str("paste \"")?;
                write_hex_escape(f, text)?;
                f.write_str("\"")
            }
            Event::PasteEnd => f.write_str("paste end"),
            Event::Focus(in_) => f.write_str(if *in_ { "focus in" } else { "focus out" }),
            Event::Mouse(m) => {
                write!(
                    f,
                    "mouse {} {} row={} col={}",
                    m.button.tag_name(),
                    m.action.tag_name(),
                    m.row,
                    m.col
                )?;
                if !m.mods.is_empty() {
                    let mut buf = Vec::new();
                    write_mods(m.mods, &mut buf);
                    f.write_str(&String::from_utf8_lossy(&buf))?;
                }
                Ok(())
            }
            Event::Unknown(bytes) => {
                f.write_str("unknown \"")?;
                write_hex_escape(f, bytes)?;
                f.write_str("\"")
            }
        }
    }
}

/// True if `bytes` hold nothing but focus reports (`CSI I`, `CSI O`).
pub fn only_focus_reports(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while !rest.is_empty() {
        if !rest.starts_with(b"\x1b[I") && !rest.starts_with(b"\x1b[O") {
            return false;
        }
        rest = &rest[3..];
    }
    true
}

/// Writes a key in Neovim's key notation, the form `nvim_input` takes:
/// `a`, `<lt>`, `<CR>`, `<C-Right>`, `<M-x>`, `<C-Space>`.
pub fn write_nvim(event: KeyEvent, w: &mut Vec<u8>) {
    // Keys that need a name inside <...>.
    let name: Option<&str> = match event.key {
        Key::Named(n) => Some(nvim_name(n)),
        Key::Codepoint(cp) => match cp {
            // a bare < would start a key name
            0x3c => Some("lt"),
            0x20 => {
                if event.mods.is_empty() {
                    None
                } else {
                    Some("Space")
                }
            }
            _ => None,
        },
    };
    let bracket = name.is_some() || !event.mods.is_empty();
    if bracket {
        w.push(b'<');
    }
    if event.mods.ctrl {
        w.extend_from_slice(b"C-");
    }
    if event.mods.alt {
        w.extend_from_slice(b"M-");
    }
    if event.mods.shift {
        w.extend_from_slice(b"S-");
    }
    if event.mods.super_ {
        w.extend_from_slice(b"D-");
    }
    if let Some(n) = name {
        w.extend_from_slice(n.as_bytes());
    } else {
        let Key::Codepoint(cp) = event.key else {
            unreachable!("named keys always have a name")
        };
        // Not encodable (a lone surrogate): send U+FFFD instead.
        let c = char::from_u32(cp).unwrap_or('\u{fffd}');
        let mut buf = [0u8; 4];
        w.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    if bracket {
        w.push(b'>');
    }
}

fn nvim_name(n: Named) -> &'static str {
    match n {
        Named::Enter => "CR",
        Named::Tab => "Tab",
        Named::Backspace => "BS",
        Named::Escape => "Esc",
        Named::Up => "Up",
        Named::Down => "Down",
        Named::Left => "Left",
        Named::Right => "Right",
        Named::Home => "Home",
        Named::End => "End",
        Named::Insert => "Insert",
        Named::Delete => "Del",
        Named::PageUp => "PageUp",
        Named::PageDown => "PageDown",
        Named::F1 => "F1",
        Named::F2 => "F2",
        Named::F3 => "F3",
        Named::F4 => "F4",
        Named::F5 => "F5",
        Named::F6 => "F6",
        Named::F7 => "F7",
        Named::F8 => "F8",
        Named::F9 => "F9",
        Named::F10 => "F10",
        Named::F11 => "F11",
        Named::F12 => "F12",
    }
}

fn write_mods(m: Mods, w: &mut Vec<u8>) {
    w.extend_from_slice(b" (");
    let mut first = true;
    for (set, field) in [
        (m.ctrl, "ctrl"),
        (m.alt, "alt"),
        (m.shift, "shift"),
        (m.super_, "super"),
    ] {
        if set {
            if !first {
                w.push(b'+');
            }
            w.extend_from_slice(field.as_bytes());
            first = false;
        }
    }
    w.push(b')');
}

const MAX_PARAMS: usize = 16;
const MAX_SEQUENCE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between keys.
    Ground,
    /// Partway through a multi-byte UTF-8 character.
    Utf8,
    /// Just read ESC.
    Escape,
    /// Inside ESC [ ... (a CSI sequence), reading parameters.
    Csi,
    /// Just read ESC O (an SS3 sequence); the next byte finishes it.
    Ss3,
    /// Inside ESC ] ... (an OSC string): terminal replies, not keys.
    Osc,
    /// Read ESC inside an OSC string: `\` ends it.
    OscEscape,
}

/// An event before its byte slices are resolved. The state machine can't
/// return `Event<'_>` borrowed from `&mut self` out of a loop (the borrow
/// would outlive the loop's later uses of `self`), so slices are named by
/// where they live and turned into slices in `Parser::resolve`.
#[derive(Debug, Clone, Copy)]
enum Raw {
    /// An event with no slice borrowed from the parser.
    Event(Event<'static>),
    /// `Paste` of `input[start..end]`.
    PasteInput(usize, usize),
    /// `Paste` of the escape sequence read so far.
    PasteSequence,
    /// `Unknown` with the escape sequence read so far.
    UnknownSequence,
}

// Clone: `Forwarder::takes_focus` reads from a copy (the Zig copies the struct).
#[derive(Clone)]
pub struct Parser {
    state: State,

    /// The bytes from the last `feed`, and how far through them we are.
    input: Vec<u8>,
    pos: usize,

    /// The escape sequence being read, for `Unknown` events and pastes.
    sequence: [u8; MAX_SEQUENCE],
    sequence_len: usize,

    /// CSI parameters, in order. `ESC [ 1 ; 5 C` has the values 1 and 5.
    /// Parameters can have sub-parameters after a `:`, as in kitty's
    /// `ESC [ 49 : 33 ; 4 u`: those are stored in the same list, with their
    /// bit set in `is_sub`. So that one is 49, 33 (sub), 4. Use `field` to
    /// read them.
    params: [u32; MAX_PARAMS],
    is_sub: u16,
    param_count: usize,
    /// The value being read, whether it has any digits yet, and whether a
    /// `:` came before it.
    current: u32,
    has_current: bool,
    current_is_sub: bool,
    /// A marker right after `ESC [`, such as `<` for SGR mouse reports.
    marker: u8,

    /// A UTF-8 character being collected, and how many bytes it still needs.
    codepoint: u32,
    utf8_left: u8,

    /// A lone ESC was followed by a key: that key gets Alt.
    alt: bool,
    /// Inside a bracketed paste.
    in_paste: bool,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser {
            state: State::Ground,
            input: Vec::new(),
            pos: 0,
            sequence: [0; MAX_SEQUENCE],
            sequence_len: 0,
            params: [0; MAX_PARAMS],
            is_sub: 0,
            param_count: 0,
            current: 0,
            has_current: false,
            current_is_sub: false,
            marker: 0,
            codepoint: 0,
            utf8_left: 0,
            alt: false,
            in_paste: false,
        }
    }
}

fn key_raw(key: Key, mods: Mods) -> Raw {
    Raw::Event(Event::Key(KeyEvent { key, mods }))
}

impl Parser {
    /// Give the parser the bytes from one read. Call `next` until it returns
    /// `None` before feeding more.
    pub fn feed(&mut self, bytes: &[u8]) {
        debug_assert!(self.pos >= self.input.len());
        self.input.clear();
        self.input.extend_from_slice(bytes);
        self.pos = 0;
    }

    /// Gives up the fed bytes not read yet and returns them, so the caller
    /// can feed them again later. Call between events. If the event ended at
    /// an ESC that starts the next one, that ESC is given back too.
    pub fn take_rest(&mut self) -> Vec<u8> {
        if self.state == State::Escape {
            self.state = State::Ground;
            self.pos -= 1;
        }
        let rest = self.input[self.pos..].to_vec();
        self.input.clear();
        self.pos = 0;
        rest
    }

    /// Whether the parser is between inputs: not partway through an escape
    /// sequence or character (the Zig reads `parser.state == .ground`).
    pub fn at_ground(&self) -> bool {
        self.state == State::Ground
    }

    /// Puts the parser partway through a CSI sequence, or back between keys:
    /// the Zig tests set `parser.state` directly, which is private here.
    #[cfg(test)]
    pub fn set_partway_for_test(&mut self, partway: bool) {
        self.state = if partway { State::Csi } else { State::Ground };
    }

    /// The next event from the fed bytes, or `None` when they are used up.
    // Not an `Iterator`: the events borrow from the parser.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Event<'_>> {
        let raw = self.next_raw()?;
        Some(self.resolve(raw))
    }

    /// Turns a `Raw` into the event it stands for.
    fn resolve(&self, raw: Raw) -> Event<'_> {
        match raw {
            Raw::Event(event) => event,
            Raw::PasteInput(start, end) => Event::Paste(&self.input[start..end]),
            Raw::PasteSequence => Event::Paste(&self.sequence[..self.sequence_len]),
            Raw::UnknownSequence => Event::Unknown(&self.sequence[..self.sequence_len]),
        }
    }

    fn next_raw(&mut self) -> Option<Raw> {
        while self.pos < self.input.len() {
            // Inside a paste, plain bytes pass through in runs; only ESC
            // (which might start the end-of-paste marker) needs a closer look.
            if self.in_paste && self.state == State::Ground {
                let rest = &self.input[self.pos..];
                let run = rest.iter().position(|&b| b == 0x1b).unwrap_or(rest.len());
                if run > 0 {
                    let start = self.pos;
                    self.pos += run;
                    return Some(Raw::PasteInput(start, start + run));
                }
            }
            let byte = self.input[self.pos];
            self.pos += 1;
            if let Some(event) = self.step(byte) {
                return Some(event);
            }
        }
        // The read ended right after an ESC. Escape sequences arrive in one
        // read, so this was the Escape key itself.
        if self.state == State::Escape {
            self.state = State::Ground;
            if self.in_paste {
                return Some(Raw::Event(Event::Paste(b"\x1b")));
            }
            return Some(self.key_event(Key::Named(Named::Escape), Mods::default()));
        }
        None
    }

    /// Advance the state machine by one byte; returns an event if one
    /// finished.
    fn step(&mut self, byte: u8) -> Option<Raw> {
        match self.state {
            State::Ground => self.ground(byte),
            State::Utf8 => {
                if byte & 0xc0 != 0x80 {
                    // Not a continuation byte: the character was cut short.
                    // Report that, then read this byte again from `ground`.
                    self.state = State::Ground;
                    self.pos -= 1;
                    return Some(self.key_event(Key::Codepoint(0xfffd), Mods::default()));
                }
                self.codepoint = (self.codepoint << 6) | u32::from(byte & 0x3f);
                self.utf8_left -= 1;
                if self.utf8_left > 0 {
                    return None;
                }
                self.state = State::Ground;
                Some(self.key_event(Key::Codepoint(self.codepoint), Mods::default()))
            }
            State::Escape => {
                match byte {
                    b'[' => {
                        self.record(byte);
                        self.state = State::Csi;
                        self.param_count = 0;
                        self.is_sub = 0;
                        self.current = 0;
                        self.has_current = false;
                        self.current_is_sub = false;
                        self.marker = 0;
                    }
                    b'O' => {
                        self.record(byte);
                        self.state = State::Ss3;
                    }
                    b']' => {
                        self.record(byte);
                        self.state = State::Osc;
                    }
                    0x1b => {
                        // Two ESCs: the first was the Escape key; the second
                        // starts something new.
                        self.start_sequence();
                        if self.in_paste {
                            return Some(Raw::Event(Event::Paste(b"\x1b")));
                        }
                        return Some(self.key_event(Key::Named(Named::Escape), Mods::default()));
                    }
                    _ => {
                        // ESC followed by a key is Alt + that key. Read the
                        // key again from `ground`, with Alt pending.
                        self.state = State::Ground;
                        self.pos -= 1;
                        if self.in_paste {
                            return Some(Raw::Event(Event::Paste(b"\x1b")));
                        }
                        self.alt = true;
                    }
                }
                None
            }
            State::Csi => {
                self.record(byte);
                match byte {
                    b'0'..=b'9' => {
                        // Saturating maths: a silly long number can't overflow.
                        self.current = self
                            .current
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte - b'0'));
                        self.has_current = true;
                    }
                    b':' => {
                        self.push_param();
                        self.current_is_sub = true;
                    }
                    b';' => {
                        self.push_param();
                        self.current_is_sub = false;
                    }
                    // A marker straight after `ESC [`, like the `<` of mouse
                    // reports or the `?` of terminal replies.
                    b'<' | b'=' | b'>' | b'?' => {
                        if self.sequence_len == 3 {
                            self.marker = byte;
                        }
                    }
                    // Intermediate bytes: none of the keys we handle use them.
                    0x20..=0x2f => {}
                    // The final byte ends the sequence and says what it is.
                    0x40..=0x7e => {
                        if self.has_current || self.param_count > 0 {
                            self.push_param();
                        }
                        self.state = State::Ground;
                        return self.dispatch_csi(byte);
                    }
                    // A new ESC means this sequence was broken; start again.
                    0x1b => self.start_sequence(),
                    _ => {} // other control bytes are ignored
                }
                None
            }
            State::Ss3 => {
                self.record(byte);
                self.state = State::Ground;
                if self.in_paste {
                    return Some(Raw::PasteSequence);
                }
                let named = match byte {
                    b'A' => Some(Named::Up),
                    b'B' => Some(Named::Down),
                    b'C' => Some(Named::Right),
                    b'D' => Some(Named::Left),
                    b'H' => Some(Named::Home),
                    b'F' => Some(Named::End),
                    b'P' => Some(Named::F1),
                    b'Q' => Some(Named::F2),
                    b'R' => Some(Named::F3),
                    b'S' => Some(Named::F4),
                    _ => None,
                };
                if let Some(n) = named {
                    return Some(self.key_event(Key::Named(n), Mods::default()));
                }
                Some(Raw::UnknownSequence)
            }
            State::Osc => {
                self.record(byte);
                match byte {
                    0x07 => return Some(self.end_osc()), // BEL ends it
                    0x1b => self.state = State::OscEscape,
                    _ => {}
                }
                None
            }
            State::OscEscape => {
                self.record(byte);
                if byte == b'\\' {
                    return Some(self.end_osc()); // ESC \ ends it
                }
                self.state = State::Osc;
                None
            }
        }
    }

    fn ground(&mut self, byte: u8) -> Option<Raw> {
        let none = Mods::default();
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        match byte {
            0x1b => {
                self.start_sequence();
                None
            }
            0x0d => Some(self.key_event(Key::Named(Named::Enter), none)),
            0x09 => Some(self.key_event(Key::Named(Named::Tab), none)),
            0x7f => Some(self.key_event(Key::Named(Named::Backspace), none)),
            0x00 => Some(self.key_event(Key::Codepoint(u32::from(b' ')), ctrl)),
            // Ctrl+A .. Ctrl+Z (Tab and Enter, 0x09 and 0x0d, are above).
            0x01..=0x08 | 0x0a..=0x0c | 0x0e..=0x1a => {
                Some(self.key_event(Key::Codepoint(u32::from(b'a' + byte - 1)), ctrl))
            }
            // Ctrl+\ ] ^ _
            0x1c..=0x1f => Some(self.key_event(
                Key::Codepoint(u32::from(b"\\]^_"[usize::from(byte - 0x1c)])),
                ctrl,
            )),
            0x20..=0x7e => Some(self.key_event(Key::Codepoint(u32::from(byte)), none)),
            // The first byte of a multi-byte UTF-8 character says its length.
            0xc0..=0xff => {
                // Zig's `std.unicode.utf8ByteSequenceLength`.
                let len = match byte.leading_ones() {
                    2 => 2,
                    3 => 3,
                    4 => 4,
                    _ => return Some(self.key_event(Key::Codepoint(0xfffd), none)),
                };
                self.codepoint = u32::from(byte & (0x7fu8 >> len));
                self.utf8_left = len - 1;
                self.state = State::Utf8;
                None
            }
            // A continuation byte with no start: invalid UTF-8.
            0x80..=0xbf => Some(self.key_event(Key::Codepoint(0xfffd), none)),
        }
    }

    fn dispatch_csi(&mut self, final_: u8) -> Option<Raw> {
        // Inside a paste, only the end-of-paste marker means anything; any
        // other sequence is part of the pasted text.
        if self.in_paste {
            if self.marker == 0 && final_ == b'~' && self.param(0, 0) == 201 {
                self.in_paste = false;
                return Some(Raw::Event(Event::PasteEnd));
            }
            return Some(Raw::PasteSequence);
        }

        if self.marker == b'<' && (final_ == b'M' || final_ == b'm') {
            return Some(self.mouse(final_ == b'm'));
        }
        if self.marker != 0 {
            return Some(Raw::UnknownSequence);
        }

        let mods = Mods::from_param(self.param(1, 1));
        let named = match final_ {
            b'A' => Some(Named::Up),
            b'B' => Some(Named::Down),
            b'C' => Some(Named::Right),
            b'D' => Some(Named::Left),
            b'H' => Some(Named::Home),
            b'F' => Some(Named::End),
            b'P' => Some(Named::F1),
            b'Q' => Some(Named::F2),
            b'R' => Some(Named::F3),
            b'S' => Some(Named::F4),
            b'~' => match self.param(0, 0) {
                1 | 7 => Some(Named::Home),
                2 => Some(Named::Insert),
                3 => Some(Named::Delete),
                4 | 8 => Some(Named::End),
                5 => Some(Named::PageUp),
                6 => Some(Named::PageDown),
                11 => Some(Named::F1),
                12 => Some(Named::F2),
                13 => Some(Named::F3),
                14 => Some(Named::F4),
                15 => Some(Named::F5),
                17 => Some(Named::F6),
                18 => Some(Named::F7),
                19 => Some(Named::F8),
                20 => Some(Named::F9),
                21 => Some(Named::F10),
                23 => Some(Named::F11),
                24 => Some(Named::F12),
                200 => {
                    self.in_paste = true;
                    return Some(Raw::Event(Event::PasteStart));
                }
                _ => None,
            },
            b'Z' => {
                return Some(self.key_event(
                    Key::Named(Named::Tab),
                    Mods {
                        shift: true,
                        ..Mods::default()
                    },
                ));
            }
            b'I' => return Some(Raw::Event(Event::Focus(true))),
            b'O' => return Some(Raw::Event(Event::Focus(false))),
            b'u' => return self.csi_u(),
            _ => None,
        };
        if let Some(n) = named {
            return Some(self.key_event(Key::Named(n), mods));
        }
        Some(Raw::UnknownSequence)
    }

    /// A kitty keyboard protocol key: `ESC [ code : shifted ; mods : event u`.
    /// `code` is the key without Shift (`a`, `1`); `shifted` is what Shift
    /// makes it (`A`, `!`), sent only when Shift is held.
    fn csi_u(&mut self) -> Option<Raw> {
        let code = self.field(0, 0, 0);
        let shifted = self.field(0, 1, 0);
        let mut mods = Mods::from_param(self.field(1, 0, 1));

        // Event type: 1 press, 2 repeat, 3 release. We only ask for presses,
        // but drop a release if one turns up rather than act on it twice.
        if self.field(1, 1, 1) == 3 {
            return None;
        }

        if let Some(key) = kitty_key(code) {
            return Some(self.key_event(key, mods));
        }
        // Other codes in Unicode's private use area are keys we don't
        // handle: F13 and up, media keys, the modifier keys themselves.
        if (0xe000..=0xf8ff).contains(&code) {
            return Some(Raw::UnknownSequence);
        }
        if code > 0x10ffff {
            return Some(Raw::UnknownSequence);
        }

        // Shift+key without Ctrl or Super is just the shifted character:
        // Alt+Shift+1 is `<M-!>`, as Neovim expects. With Ctrl, keep the
        // base key and Shift, so Ctrl+Shift+A stays `<C-S-a>` and doesn't
        // collapse into Ctrl+A.
        let mut cp = code;
        if mods.shift && !mods.ctrl && !mods.super_ && shifted != 0 && shifted <= 0x10ffff {
            cp = shifted;
            mods.shift = false;
        }
        Some(self.key_event(Key::Codepoint(cp), mods))
    }

    /// SGR mouse report: `ESC [ < button ; col ; row M` (press) or `m`
    /// (release). Columns and rows count from 1.
    fn mouse(&self, release: bool) -> Raw {
        let cb = self.param(0, 0);
        let col = (self.param(1, 1) - 1).min(u32::from(u16::MAX)) as u16;
        let row = (self.param(2, 1) - 1).min(u32::from(u16::MAX)) as u16;
        // Bits 4, 8 and 16 of the button number are the modifiers.
        let mods = Mods {
            shift: cb & 4 != 0,
            alt: cb & 8 != 0,
            ctrl: cb & 16 != 0,
            super_: false,
        };
        let low = (cb & 3) as usize;
        // Bit 64: a wheel, not a button.
        if cb & 64 != 0 {
            let wheels = [
                MouseButton::WheelUp,
                MouseButton::WheelDown,
                MouseButton::WheelLeft,
                MouseButton::WheelRight,
            ];
            return Raw::Event(Event::Mouse(Mouse {
                button: wheels[low],
                action: MouseAction::Press,
                mods,
                row,
                col,
            }));
        }
        let buttons = [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            MouseButton::None,
        ];
        let button = buttons[low];
        // Bit 32: the pointer moved (with a button held, that's a drag).
        let action = if release {
            MouseAction::Release
        } else if cb & 32 != 0 {
            if button == MouseButton::None {
                MouseAction::Motion
            } else {
                MouseAction::Drag
            }
        } else {
            MouseAction::Press
        };
        Raw::Event(Event::Mouse(Mouse {
            button,
            action,
            mods,
            row,
            col,
        }))
    }

    fn end_osc(&mut self) -> Raw {
        self.state = State::Ground;
        if self.in_paste {
            return Raw::PasteSequence;
        }
        Raw::UnknownSequence
    }

    /// CSI parameter `i` (ignoring sub-parameters), or `default` when it is
    /// missing or 0.
    fn param(&self, i: usize, default: u32) -> u32 {
        self.field(i, 0, default)
    }

    fn is_sub_set(&self, i: usize) -> bool {
        self.is_sub & (1 << i) != 0
    }

    /// Sub-parameter `sub` of parameter `group`, or `default` when it is
    /// missing or 0. In `ESC [ 49 : 33 ; 4 u`, field(0, 0) is 49,
    /// field(0, 1) is 33 and field(1, 0) is 4.
    fn field(&self, group: usize, sub: usize, default: u32) -> u32 {
        // Find where the group starts: every value that isn't a
        // sub-parameter starts a new group.
        let mut g = 0;
        let mut start = 0;
        let mut found = false;
        while start < self.param_count {
            if start > 0 && !self.is_sub_set(start) {
                g += 1;
            }
            if g == group {
                found = true;
                break;
            }
            start += 1;
        }
        if !found {
            return default;
        }

        // Each value up to the one we want must belong to this group.
        let i = start + sub;
        if i >= self.param_count {
            return default;
        }
        for j in start + 1..i + 1 {
            if !self.is_sub_set(j) {
                return default;
            }
        }
        if self.params[i] == 0 {
            default
        } else {
            self.params[i]
        }
    }

    fn push_param(&mut self) {
        if self.param_count < MAX_PARAMS {
            self.params[self.param_count] = self.current;
            if self.current_is_sub {
                self.is_sub |= 1 << self.param_count;
            } else {
                self.is_sub &= !(1 << self.param_count);
            }
            self.param_count += 1;
        }
        self.current = 0;
        self.has_current = false;
    }

    /// A new escape sequence starts with this ESC.
    fn start_sequence(&mut self) {
        self.state = State::Escape;
        self.sequence[0] = 0x1b;
        self.sequence_len = 1;
    }

    /// Keep a sequence byte, for `Unknown` events. Very long sequences are
    /// cut short; they are never keys.
    fn record(&mut self, byte: u8) {
        if self.sequence_len < MAX_SEQUENCE {
            self.sequence[self.sequence_len] = byte;
            self.sequence_len += 1;
        }
    }

    /// A key event, with Alt added if a lone ESC came just before.
    fn key_event(&mut self, key: Key, mods: Mods) -> Raw {
        let mut m = mods;
        if self.alt {
            m.alt = true;
            self.alt = false;
        }
        key_raw(key, m)
    }
}

/// Kitty key codes that aren't plain characters. Codes from 57344 up
/// are in Unicode's private use area; the keypad ones become the
/// character or key they type.
fn kitty_key(code: u32) -> Option<Key> {
    Some(match code {
        13 | 57414 => Key::Named(Named::Enter),
        9 => Key::Named(Named::Tab),
        27 => Key::Named(Named::Escape),
        127 => Key::Named(Named::Backspace),
        57399..=57408 => Key::Codepoint(u32::from(b'0') + (code - 57399)), // keypad 0-9
        57409 => Key::Codepoint(u32::from(b'.')),
        57410 => Key::Codepoint(u32::from(b'/')),
        57411 => Key::Codepoint(u32::from(b'*')),
        57412 => Key::Codepoint(u32::from(b'-')),
        57413 => Key::Codepoint(u32::from(b'+')),
        57415 => Key::Codepoint(u32::from(b'=')),
        57416 => Key::Codepoint(u32::from(b',')),
        57417 => Key::Named(Named::Left),
        57418 => Key::Named(Named::Right),
        57419 => Key::Named(Named::Up),
        57420 => Key::Named(Named::Down),
        57421 => Key::Named(Named::PageUp),
        57422 => Key::Named(Named::PageDown),
        57423 => Key::Named(Named::Home),
        57424 => Key::Named(Named::End),
        57425 => Key::Named(Named::Insert),
        57426 => Key::Named(Named::Delete),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed each of `feeds` in turn (as separate reads) and check the events.
    fn expect_events(feeds: &[&[u8]], want: &[Event]) {
        let mut parser = Parser::default();
        let mut i = 0;
        for bytes in feeds {
            parser.feed(bytes);
            while let Some(event) = parser.next() {
                assert!(i < want.len(), "unexpected extra event: {event}");
                // Compare straight away: events can point into parser memory
                // that the next event reuses.
                assert_eq!(want[i], event);
                i += 1;
            }
        }
        assert_eq!(want.len(), i);
    }

    fn test_key(cp: char, mods: Mods) -> Event<'static> {
        Event::Key(KeyEvent {
            key: Key::Codepoint(cp as u32),
            mods,
        })
    }

    fn test_named(n: Named, mods: Mods) -> Event<'static> {
        Event::Key(KeyEvent {
            key: Key::Named(n),
            mods,
        })
    }

    const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
        super_: false,
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };

    #[test]
    fn focus_reports_alone() {
        assert!(only_focus_reports(b"\x1b[O"));
        assert!(only_focus_reports(b"\x1b[I\x1b[O"));
        assert!(!only_focus_reports(b"\x1b[Ia"));
        assert!(!only_focus_reports(b"\x1b[A"));
    }

    #[test]
    fn characters() {
        expect_events(&[b"a"], &[test_key('a', NONE)]);
        expect_events(
            &[b"aZ5"],
            &[
                test_key('a', NONE),
                test_key('Z', NONE),
                test_key('5', NONE),
            ],
        );
        expect_events(&["é".as_bytes()], &[test_key('é', NONE)]);
        expect_events(&["漢".as_bytes()], &[test_key('漢', NONE)]);
        expect_events(&["🙂".as_bytes()], &[test_key('🙂', NONE)]);
    }

    #[test]
    fn control_keys() {
        expect_events(&[b"\r"], &[test_named(Named::Enter, NONE)]);
        expect_events(&[b"\t"], &[test_named(Named::Tab, NONE)]);
        expect_events(&[b"\x7f"], &[test_named(Named::Backspace, NONE)]);
        expect_events(&[b"\x01"], &[test_key('a', CTRL)]);
        expect_events(&[b"\x08"], &[test_key('h', CTRL)]);
        expect_events(&[b"\x00"], &[test_key(' ', CTRL)]);
        expect_events(&[b"\x1c"], &[test_key('\\', CTRL)]);
    }

    #[test]
    fn escape_and_alt() {
        expect_events(&[b"\x1b"], &[test_named(Named::Escape, NONE)]);
        expect_events(&[b"\x1bx"], &[test_key('x', ALT)]);
        expect_events(&["\x1bé".as_bytes()], &[test_key('é', ALT)]);
        expect_events(
            &[b"\x1b\x01"],
            &[test_key(
                'a',
                Mods {
                    ctrl: true,
                    alt: true,
                    ..NONE
                },
            )],
        );
        expect_events(
            &[b"\x1b\x1b"],
            &[
                test_named(Named::Escape, NONE),
                test_named(Named::Escape, NONE),
            ],
        );
        // A lone ESC at the end of one read, then a key in the next: two keys.
        expect_events(
            &[b"\x1b", b"x"],
            &[test_named(Named::Escape, NONE), test_key('x', NONE)],
        );
    }

    #[test]
    fn csi_and_ss3_keys() {
        expect_events(&[b"\x1b[A"], &[test_named(Named::Up, NONE)]);
        expect_events(&[b"\x1b[1;5C"], &[test_named(Named::Right, CTRL)]);
        expect_events(
            &[b"\x1b[1;4D"],
            &[test_named(
                Named::Left,
                Mods {
                    shift: true,
                    alt: true,
                    ..NONE
                },
            )],
        );
        // Alt on function keys is a modifier param, as Ghostty sends it
        expect_events(&[b"\x1b[1;3A"], &[test_named(Named::Up, ALT)]);
        // ESC prefix only means Alt for text keys, as in Ghostty
        expect_events(
            &[b"\x1b\x1b[A"],
            &[test_named(Named::Escape, NONE), test_named(Named::Up, NONE)],
        );
        expect_events(&[b"\x1b[3~"], &[test_named(Named::Delete, NONE)]);
        expect_events(&[b"\x1b[15~"], &[test_named(Named::F5, NONE)]);
        expect_events(&[b"\x1b[24;2~"], &[test_named(Named::F12, SHIFT)]);
        expect_events(&[b"\x1bOP"], &[test_named(Named::F1, NONE)]);
        expect_events(&[b"\x1bOA"], &[test_named(Named::Up, NONE)]);
        expect_events(&[b"\x1b[Z"], &[test_named(Named::Tab, SHIFT)]);
        expect_events(&[b"\x1b[97;5u"], &[test_key('a', CTRL)]);
        expect_events(&[b"\x1b[13;2u"], &[test_named(Named::Enter, SHIFT)]);
    }

    #[test]
    fn kitty_keyboard_protocol() {
        // Keys the legacy encoding can't tell apart.
        expect_events(&[b"\x1b[27u"], &[test_named(Named::Escape, NONE)]);
        expect_events(&[b"\x1b[105;5u"], &[test_key('i', CTRL)]);
        expect_events(&[b"\x1b[104;5u"], &[test_key('h', CTRL)]);
        expect_events(&[b"\x1b[120;3u"], &[test_key('x', ALT)]);
        expect_events(&[b"\x1b[13;5u"], &[test_named(Named::Enter, CTRL)]);
        // Shift: use the shifted key, unless Ctrl is held too.
        expect_events(&[b"\x1b[49:33;4u"], &[test_key('!', ALT)]);
        expect_events(
            &[b"\x1b[97:65;6u"],
            &[test_key(
                'a',
                Mods {
                    ctrl: true,
                    shift: true,
                    ..NONE
                },
            )],
        );
        // Caps lock (64) and num lock (128) bits are ignored: 1 + 4 + 64.
        expect_events(&[b"\x1b[97;69u"], &[test_key('a', CTRL)]);
        // A release (event type 3) makes no event.
        expect_events(&[b"\x1b[97;5:3u"], &[]);
        // Keypad keys, and keys we don't handle.
        expect_events(
            &[b"\x1b[57399u\x1b[57414u"],
            &[test_key('0', NONE), test_named(Named::Enter, NONE)],
        );
        expect_events(&[b"\x1b[57441;2u"], &[Event::Unknown(b"\x1b[57441;2u")]);
        // Legacy sequences can carry an event type too.
        expect_events(&[b"\x1b[1;5:1C"], &[test_named(Named::Right, CTRL)]);
    }

    #[test]
    fn sequences_split_across_reads() {
        expect_events(&[b"\x1b[1;", b"5C"], &[test_named(Named::Right, CTRL)]);
        expect_events(&[b"\x1b[", b"A"], &[test_named(Named::Up, NONE)]);
        expect_events(&[b"\xc3", b"\xa9"], &[test_key('é', NONE)]);
    }

    #[test]
    fn taking_the_rest_after_a_key_gives_back_an_esc_that_starts_the_next() {
        let mut parser = Parser::default();
        parser.feed(b"\x1b\x1b");
        assert!(matches!(
            parser.next().unwrap(),
            Event::Key(KeyEvent {
                key: Key::Named(Named::Escape),
                ..
            })
        ));
        assert_eq!(b"\x1b".to_vec(), parser.take_rest());
        // read again from ground: a lone ESC at the end of a read is Escape, so
        // the next key isn't Alt
        parser.feed(b"\x1b");
        assert!(matches!(
            parser.next().unwrap(),
            Event::Key(KeyEvent {
                key: Key::Named(Named::Escape),
                ..
            })
        ));
        assert!(parser.next().is_none());
        parser.feed(b"x");
        let Event::Key(k) = parser.next().unwrap() else {
            panic!("expected a key")
        };
        assert_eq!(Mods::default(), k.mods);

        parser.feed(b"ab");
        let _ = parser.next();
        assert_eq!(b"b".to_vec(), parser.take_rest());
    }

    #[test]
    fn unknown_and_broken_input() {
        expect_events(&[b"\x1b[99X"], &[Event::Unknown(b"\x1b[99X")]);
        expect_events(
            &[b"\x1b]11;rgb:0/0/0\x07a"],
            &[
                Event::Unknown(b"\x1b]11;rgb:0/0/0\x07"),
                test_key('a', NONE),
            ],
        );
        expect_events(
            &[b"\xc3a"],
            &[test_key('\u{fffd}', NONE), test_key('a', NONE)],
        );
        expect_events(&[b"\x80"], &[test_key('\u{fffd}', NONE)]);
    }

    fn test_mouse(
        button: MouseButton,
        action: MouseAction,
        mods: Mods,
        row: u16,
        col: u16,
    ) -> Event<'static> {
        Event::Mouse(Mouse {
            button,
            action,
            mods,
            row,
            col,
        })
    }

    #[test]
    fn paste_focus_and_mouse() {
        expect_events(
            &[b"\x1b[200~hi\r\x1b[201~"],
            &[Event::PasteStart, Event::Paste(b"hi\r"), Event::PasteEnd],
        );
        expect_events(
            &[b"\x1b[200~a", b"b\x1b[201~"],
            &[
                Event::PasteStart,
                Event::Paste(b"a"),
                Event::Paste(b"b"),
                Event::PasteEnd,
            ],
        );
        expect_events(
            &[b"\x1b[200~\x1b[A\x1b[201~"],
            &[Event::PasteStart, Event::Paste(b"\x1b[A"), Event::PasteEnd],
        );
        expect_events(
            &[b"\x1b[I\x1b[O"],
            &[Event::Focus(true), Event::Focus(false)],
        );
        expect_events(
            &[b"\x1b[<0;10;5M"],
            &[test_mouse(
                MouseButton::Left,
                MouseAction::Press,
                NONE,
                4,
                9,
            )],
        );
        expect_events(
            &[b"\x1b[<0;10;5m"],
            &[test_mouse(
                MouseButton::Left,
                MouseAction::Release,
                NONE,
                4,
                9,
            )],
        );
        expect_events(
            &[b"\x1b[<64;1;1M"],
            &[test_mouse(
                MouseButton::WheelUp,
                MouseAction::Press,
                NONE,
                0,
                0,
            )],
        );
        expect_events(
            &[b"\x1b[<48;3;2M"],
            &[test_mouse(MouseButton::Left, MouseAction::Drag, CTRL, 1, 2)],
        );
    }

    #[test]
    fn neovim_notation() {
        let cases = [
            (KeyEvent::new(Key::Codepoint('a' as u32)), "a"),
            (KeyEvent::new(Key::Codepoint('<' as u32)), "<lt>"),
            (KeyEvent::new(Key::Codepoint(' ' as u32)), " "),
            (
                KeyEvent {
                    key: Key::Codepoint(' ' as u32),
                    mods: CTRL,
                },
                "<C-Space>",
            ),
            (
                KeyEvent {
                    key: Key::Codepoint('x' as u32),
                    mods: ALT,
                },
                "<M-x>",
            ),
            (KeyEvent::new(Key::Codepoint('é' as u32)), "é"),
            (KeyEvent::new(Key::Named(Named::Enter)), "<CR>"),
            (
                KeyEvent {
                    key: Key::Named(Named::Right),
                    mods: CTRL,
                },
                "<C-Right>",
            ),
            (
                KeyEvent {
                    key: Key::Named(Named::F5),
                    mods: SHIFT,
                },
                "<S-F5>",
            ),
            (
                KeyEvent {
                    key: Key::Codepoint('i' as u32),
                    mods: CTRL,
                },
                "<C-i>",
            ),
            (
                KeyEvent {
                    key: Key::Codepoint('a' as u32),
                    mods: Mods {
                        ctrl: true,
                        shift: true,
                        ..NONE
                    },
                },
                "<C-S-a>",
            ),
            (
                KeyEvent {
                    key: Key::Codepoint('!' as u32),
                    mods: ALT,
                },
                "<M-!>",
            ),
        ];
        for (event, want) in cases {
            let mut buf = Vec::new();
            write_nvim(event, &mut buf);
            assert_eq!(want.as_bytes(), buf.as_slice());
        }
    }
}
