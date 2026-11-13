//! Editor-wide drawing state that every grid shares: highlights, default
//! colors and how the cursor looks.
//!
//! Colors are 24-bit: a `u32` below `1 << 24`.

use crate::nvim::msgpack::Decoder;
use crate::nvim::rpc;

#[derive(Debug, Clone, Default)]
#[doc(hidden)]
pub struct Style {
    /// Highlight attributes by id. Id 0 is the default.
    pub attrs: Vec<Attr>,
    /// Defaults for colors an `Attr` leaves null.
    pub default_fg: Option<u32>,
    pub default_bg: Option<u32>,
    pub default_sp: Option<u32>,
    /// Cursor looks by mode, from `mode_info_set`.
    pub cursor_modes: Vec<CursorMode>,
    /// False when 'guicursor' is empty: leave the terminal's cursor alone.
    pub cursor_style_enabled: bool,
    pub mode_idx: u32,
    /// Between busy_start and busy_stop, the cursor is hidden.
    pub busy: bool,
    /// Highlights made by blending a float over what's under it, with ids
    /// from `BLEND_BASE`. Good for one frame: cleared by `clear_blends`. At
    /// most `MAX_BLENDED`.
    pub blended: Vec<Blended>,
    /// The highlights of the statuslines neopane draws for Neovim, with ids
    /// from `STATUSLINE_BASE`: Neovim's groups combined as it would draw them,
    /// which it never defines an id for. Replaced by each evaluation.
    pub statusline_attrs: Vec<Attr>,
}

/// Ids from here are blended highlights, never Neovim's.
pub(crate) const BLEND_BASE: u32 = 1 << 31;
/// Ids from here to `BLEND_BASE` are statusline highlights, never Neovim's.
pub const STATUSLINE_BASE: u32 = 1 << 30;
const MAX_BLENDED: usize = 512;

#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct Blended {
    pub(crate) front: u32,
    pub(crate) back: u32,
    pub(crate) through: bool,
    pub(crate) attr: Attr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct CursorMode {
    pub shape: Shape,
    pub blink: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub enum Shape {
    #[default]
    Block,
    Horizontal,
    Vertical,
}

/// A highlight from `hl_attr_define`. Null colors mean the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct Attr {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub sp: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub reverse: bool,
    pub strikethrough: bool,
    pub dim: bool,
    pub blink: bool,
    pub conceal: bool,
    pub overline: bool,
    pub altfont: bool,
    pub underline: Underline,
    /// 0 to 100.
    pub blend: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub enum Underline {
    #[default]
    None,
    Single,
    Curl,
    Double,
    Dotted,
    Dashed,
}

impl Style {
    /// Highlight `hl`, or the default for an id never defined.
    pub fn attr(&self, hl: u32) -> Attr {
        if hl >= BLEND_BASE {
            let i = (hl - BLEND_BASE) as usize;
            return self.blended.get(i).map(|b| b.attr).unwrap_or_default();
        }
        if hl >= STATUSLINE_BASE {
            let i = (hl - STATUSLINE_BASE) as usize;
            return self.statusline_attrs.get(i).copied().unwrap_or_default();
        }
        self.attrs.get(hl as usize).copied().unwrap_or_default()
    }

    /// Forgets the blended highlights; call at the start of each frame.
    pub(crate) fn clear_blends(&mut self) {
        self.blended.clear();
    }

    /// Float highlight `front` over `back`, as Neovim's compositor blends them
    /// by `front`'s blend. `through`: the float's cell is blank and `back`'s
    /// text shows. Just `front` when it doesn't blend or colors are unknown.
    pub(crate) fn blend_over(&mut self, front: u32, back: u32, through: bool) -> u32 {
        let f = self.attr(front);
        if f.blend == 0 {
            return front;
        }
        for (i, b) in self.blended.iter().enumerate() {
            if b.front == front && b.back == back && b.through == through {
                return BLEND_BASE + i as u32;
            }
        }
        if self.blended.len() == MAX_BLENDED {
            return front;
        }
        let b = self.attr(back);
        let Some(fbg) = f.bg.or(self.default_bg) else {
            return front;
        };
        // null on a transparent background: it stays transparent
        let bbg = b.bg.or(self.default_bg);
        let mut a = f;
        if through {
            a = b;
            let Some(bfg) = b.fg.or(self.default_fg) else {
                return front;
            };
            a.fg = Some(mix(f.blend, bfg, fbg));
            if let Some(sp) = b.sp {
                a.sp = Some(mix(f.blend, sp, fbg));
            }
        }
        a.bg = bbg.map(|bg| mix(f.blend, bg, fbg));
        a.blend = 0;
        self.blended.push(Blended {
            front,
            back,
            through,
            attr: a,
        });
        BLEND_BASE + self.blended.len() as u32 - 1
    }

    /// How the cursor should look now; None to leave it alone.
    pub fn cursor_mode(&self) -> Option<CursorMode> {
        if !self.cursor_style_enabled || self.mode_idx as usize >= self.cursor_modes.len() {
            return None;
        }
        Some(self.cursor_modes[self.mode_idx as usize])
    }

    /// Applies one event's args, after their array header of `len`. Returns
    /// how many args it read.
    pub(crate) fn apply(
        &mut self,
        event: Event,
        d: &mut Decoder<'_>,
        len: u32,
    ) -> rpc::Result<u32> {
        match event {
            Event::HlAttrDefine => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int(d)? as usize;
                let a = read_attr(d)?;
                if id >= self.attrs.len() {
                    self.attrs.resize(id + 1, Attr::default());
                }
                self.attrs[id] = a;
                Ok(2)
            }
            Event::DefaultColorsSet => {
                if len < 3 {
                    return Err(rpc::Error::BadMessage);
                }
                self.default_fg = expect_color(d)?;
                self.default_bg = expect_color(d)?;
                self.default_sp = expect_color(d)?;
                Ok(3)
            }
            Event::ModeInfoSet => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                self.cursor_style_enabled = rpc::expect_bool(d)?;
                let n = rpc::expect_array(d)?;
                self.cursor_modes.clear();
                for _ in 0..n {
                    self.cursor_modes.push(read_cursor_mode(d)?);
                }
                Ok(2)
            }
            Event::ModeChange => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                rpc::skip(d)?; // mode name
                self.mode_idx = expect_int(d)?;
                Ok(2)
            }
            Event::BusyStart | Event::BusyStop => {
                self.busy = event == Event::BusyStart;
                Ok(0)
            }
        }
    }
}

/// `ratio` percent of `back`, the rest `front`, per channel.
fn mix(ratio: u8, back: u32, front: u32) -> u32 {
    let r = ratio.min(100) as u32;
    let mut out: u32 = 0;
    for shift in [16, 8, 0] {
        let cb = (back >> shift) & 0xff;
        let cf = (front >> shift) & 0xff;
        out |= ((r * cb + (100 - r) * cf) / 100) << shift;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    HlAttrDefine,
    DefaultColorsSet,
    ModeInfoSet,
    ModeChange,
    BusyStart,
    BusyStop,
}

/// A `mode_info_set` entry. Only shape and blink are used.
fn read_cursor_mode(d: &mut Decoder<'_>) -> rpc::Result<CursorMode> {
    let mut m = CursorMode::default();
    let mut blinkon: i64 = 0;
    let mut blinkoff: i64 = 0;
    let n = rpc::expect_map(d)?;
    for _ in 0..n {
        let key = rpc::expect_str(d)?;
        if key == b"cursor_shape" {
            m.shape = match rpc::expect_str(d)? {
                b"block" => Shape::Block,
                b"horizontal" => Shape::Horizontal,
                b"vertical" => Shape::Vertical,
                _ => Shape::Block,
            };
        } else if key == b"blinkon" {
            blinkon = rpc::expect_int(d)?;
        } else if key == b"blinkoff" {
            blinkoff = rpc::expect_int(d)?;
        } else {
            rpc::skip(d)?;
        }
    }
    // as Neovim's TUI decides
    m.blink = blinkon != 0 && blinkoff != 0;
    Ok(m)
}

/// An `rgb_attr` map. Unknown keys, such as `url`, are skipped.
pub(crate) fn read_attr(d: &mut Decoder<'_>) -> rpc::Result<Attr> {
    let mut a = Attr::default();
    let n = rpc::expect_map(d)?;
    for _ in 0..n {
        let Some(key) = AttrKey::from_name(rpc::expect_str(d)?) else {
            rpc::skip(d)?;
            continue;
        };
        match key {
            AttrKey::Foreground => a.fg = expect_color(d)?,
            AttrKey::Background => a.bg = expect_color(d)?,
            AttrKey::Special => a.sp = expect_color(d)?,
            AttrKey::Blend => {
                a.blend = u8::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)?
            }
            AttrKey::Bold => a.bold = rpc::expect_bool(d)?,
            AttrKey::Italic => a.italic = rpc::expect_bool(d)?,
            AttrKey::Reverse => a.reverse = rpc::expect_bool(d)?,
            AttrKey::Strikethrough => a.strikethrough = rpc::expect_bool(d)?,
            AttrKey::Dim => a.dim = rpc::expect_bool(d)?,
            AttrKey::Blink => a.blink = rpc::expect_bool(d)?,
            AttrKey::Conceal => a.conceal = rpc::expect_bool(d)?,
            AttrKey::Overline => a.overline = rpc::expect_bool(d)?,
            AttrKey::Altfont => a.altfont = rpc::expect_bool(d)?,
            AttrKey::Underline => {
                if rpc::expect_bool(d)? {
                    a.underline = Underline::Single;
                }
            }
            AttrKey::Undercurl => {
                if rpc::expect_bool(d)? {
                    a.underline = Underline::Curl;
                }
            }
            AttrKey::Underdouble => {
                if rpc::expect_bool(d)? {
                    a.underline = Underline::Double;
                }
            }
            AttrKey::Underdotted => {
                if rpc::expect_bool(d)? {
                    a.underline = Underline::Dotted;
                }
            }
            AttrKey::Underdashed => {
                if rpc::expect_bool(d)? {
                    a.underline = Underline::Dashed;
                }
            }
        }
    }
    // cterm_attr and info: only RGB is used
    Ok(a)
}

enum AttrKey {
    Foreground,
    Background,
    Special,
    Blend,
    Bold,
    Italic,
    Reverse,
    Strikethrough,
    Dim,
    Blink,
    Conceal,
    Overline,
    Altfont,
    Underline,
    Undercurl,
    Underdouble,
    Underdotted,
    Underdashed,
}

impl AttrKey {
    fn from_name(name: &[u8]) -> Option<AttrKey> {
        Some(match name {
            b"foreground" => AttrKey::Foreground,
            b"background" => AttrKey::Background,
            b"special" => AttrKey::Special,
            b"blend" => AttrKey::Blend,
            b"bold" => AttrKey::Bold,
            b"italic" => AttrKey::Italic,
            b"reverse" => AttrKey::Reverse,
            b"strikethrough" => AttrKey::Strikethrough,
            b"dim" => AttrKey::Dim,
            b"blink" => AttrKey::Blink,
            b"conceal" => AttrKey::Conceal,
            b"overline" => AttrKey::Overline,
            b"altfont" => AttrKey::Altfont,
            b"underline" => AttrKey::Underline,
            b"undercurl" => AttrKey::Undercurl,
            b"underdouble" => AttrKey::Underdouble,
            b"underdotted" => AttrKey::Underdotted,
            b"underdashed" => AttrKey::Underdashed,
            _ => return None,
        })
    }
}

fn expect_int(d: &mut Decoder<'_>) -> rpc::Result<u32> {
    u32::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)
}

/// An RGB color, or None for -1 (unset).
fn expect_color(d: &mut Decoder<'_>) -> rpc::Result<Option<u32>> {
    let n = rpc::expect_int(d)?;
    if n == -1 {
        return Ok(None);
    }
    if !(0..1 << 24).contains(&n) {
        return Err(rpc::Error::BadMessage);
    }
    Ok(Some(n as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float_blends_over_whats_under_it() {
        let mut style = Style {
            default_fg: Some(0xffffff),
            default_bg: Some(0x000000),
            ..Style::default()
        };
        style.attrs.extend_from_slice(&[
            Attr::default(),
            Attr {
                bg: Some(0x000000),
                blend: 60,
                ..Attr::default()
            },
            Attr {
                fg: Some(0x6464c8),
                bg: Some(0xc86464),
                ..Attr::default()
            },
            Attr {
                bg: Some(0x0000ff),
                ..Attr::default()
            },
            Attr {
                fg: Some(0xaaaaaa),
                ..Attr::default()
            },
        ]);
        // no blend: unchanged
        assert_eq!(3, style.blend_over(3, 2, false));

        let through = style.blend_over(1, 2, true);
        assert!(through >= BLEND_BASE);
        let t = style.attr(through);
        assert_eq!(Some(0x3c3c78), t.fg);
        assert_eq!(Some(0x783c3c), t.bg);
        assert_eq!(0, t.blend);
        // the same pair again: the same id
        assert_eq!(through, style.blend_over(1, 2, true));

        let id = style.blend_over(1, 2, false);
        let solid = style.attr(id);
        assert_eq!(None, solid.fg);
        assert_eq!(Some(0x783c3c), solid.bg);

        style.clear_blends();
        assert_eq!(0, style.blended.len());

        // a transparent background: text still dims, the background stays clear
        style.default_bg = None;
        let id = style.blend_over(1, 4, true);
        let clear = style.attr(id);
        assert_eq!(Some(0x666666), clear.fg);
        assert_eq!(None, clear.bg);
    }
}
