//! Pane ids, such as `12-k3j9x2qpa8`: a number and a random token. The
//! daemon mints them and remembers each, to answer an attach that names one.

use std::collections::HashMap;
use std::fmt;

use super::message::Refusal;

const TOKEN_LEN: usize = 10;
const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// A pane's id: `^(0|[1-9][0-9]*)-[a-z0-9]{10}$`, the number fitting a
/// `u32`. The token isn't a secret: the peer check authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId {
    pub number: u32,
    token: [u8; TOKEN_LEN],
}

impl PaneId {
    pub fn parse(s: &str) -> Option<Self> {
        let (number, token) = s.split_once('-')?;
        let digits_ok = !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
            && (number == "0" || !number.starts_with('0'));
        if !digits_ok {
            return None;
        }
        let token: [u8; TOKEN_LEN] = token.as_bytes().try_into().ok()?;
        if !token.iter().all(|b| ALPHABET.contains(b)) {
            return None;
        }
        Some(PaneId {
            number: number.parse().ok()?,
            token,
        })
    }

    /// The token, which the pane title uses.
    pub fn token(&self) -> &str {
        std::str::from_utf8(&self.token).expect("tokens are ASCII")
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.number, self.token())
    }
}

/// A fresh token from the OS random source.
fn token() -> [u8; TOKEN_LEN] {
    let mut out = [0u8; TOKEN_LEN];
    let mut filled = 0;
    while filled < TOKEN_LEN {
        let mut bytes = [0u8; 32];
        random(&mut bytes);
        // 252 is the largest multiple of 36 that fits: no letter is likelier
        for b in bytes.into_iter().filter(|&b| b < 252) {
            if filled == TOKEN_LEN {
                break;
            }
            out[filled] = ALPHABET[(b % 36) as usize];
            filled += 1;
        }
    }
    out
}

/// Fills `buf`, at most 256 bytes, from `getentropy`.
fn random(buf: &mut [u8]) {
    // SAFETY: `buf` is valid for writes of its length, which is at most 256.
    let r = unsafe { libc::getentropy(buf.as_mut_ptr().cast(), buf.len()) };
    // fails only on a bad pointer or a length over 256: a bug here
    assert_eq!(
        0,
        r,
        "getentropy failed: {}",
        std::io::Error::last_os_error()
    );
}

/// Where a minted id stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for its attach.
    Minted,
    Taken,
    Expired,
}

/// Every id a daemon minted, numbered from 1 and never reused.
#[derive(Debug)]
pub struct PaneIdRegistry {
    last: u32,
    ids: HashMap<u32, ([u8; TOKEN_LEN], State)>,
}

impl Default for PaneIdRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneIdRegistry {
    pub fn new() -> Self {
        PaneIdRegistry {
            last: 0,
            ids: HashMap::new(),
        }
    }

    /// A new id, good for one attach. Panics past `u32::MAX` ids.
    pub fn mint(&mut self) -> PaneId {
        self.last = self.last.checked_add(1).expect("pane ids ran out");
        let id = PaneId {
            number: self.last,
            token: token(),
        };
        self.ids.insert(id.number, (id.token, State::Minted));
        id
    }

    /// An attach takes `id`, or is refused: an id never minted (or with
    /// another token), one already taken, or one expired.
    pub fn take(&mut self, id: &PaneId) -> Result<(), Refusal> {
        self.check(id)?;
        if let Some((_, state)) = self.ids.get_mut(&id.number) {
            *state = State::Taken;
        }
        Ok(())
    }

    /// Whether an attach could take `id` now, without taking it.
    pub fn check(&self, id: &PaneId) -> Result<(), Refusal> {
        match self.ids.get(&id.number) {
            Some((token, state)) if *token == id.token => match *state {
                State::Minted => Ok(()),
                State::Taken => Err(Refusal::PaneTaken),
                State::Expired => Err(Refusal::PaneExpired),
            },
            _ => Err(Refusal::PaneUnknown),
        }
    }

    /// `id` is good for no attach: its timeout ran out, or nothing wants
    /// its pane. A taken id stays taken.
    pub fn expire(&mut self, id: &PaneId) {
        if let Some((token, state)) = self.ids.get_mut(&id.number)
            && *token == id.token
            && *state == State::Minted
        {
            *state = State::Expired;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_ids() {
        let id = PaneId::parse("12-k3j9x2qpa8").unwrap();
        assert_eq!(12, id.number);
        assert_eq!("k3j9x2qpa8", id.token());
        assert_eq!("12-k3j9x2qpa8", id.to_string());
        assert!(PaneId::parse("0-aaaaaaaaaa").is_some());
        assert_eq!(
            u32::MAX,
            PaneId::parse("4294967295-0123456789").unwrap().number
        );
    }

    #[test]
    fn parse_rejects_others() {
        for bad in [
            "012-k3j9x2qpa8",
            "00-k3j9x2qpa8",
            "4294967296-k3j9x2qpa8",
            "12-K3j9x2qpa8",
            "12-k3j9x2qpA8",
            "12-k3j9x2qpa",
            "12-k3j9x2qpa8x",
            "12-k3j9x2-pa8",
            "-k3j9x2qpa8",
            "+12-k3j9x2qpa8",
            "12_k3j9x2qpa8",
            "12-k3j9x2qpé",
            "12",
            "",
        ] {
            assert_eq!(None, PaneId::parse(bad), "{bad}");
        }
    }

    #[test]
    fn minted_ids_are_unique_and_numbered_from_1() {
        let mut reg = PaneIdRegistry::new();
        let mut seen = std::collections::HashSet::new();
        let mut tokens = std::collections::HashSet::new();
        for n in 1..=10_000u32 {
            let id = reg.mint();
            assert_eq!(n, id.number);
            assert_eq!(Some(id), PaneId::parse(&id.to_string()));
            assert!(seen.insert(id.to_string()));
            tokens.insert(id.token);
        }
        // 36^10 tokens: a repeat among 10,000 would be a broken source
        assert_eq!(10_000, tokens.len());
    }

    #[test]
    fn an_id_is_taken_once() {
        let mut reg = PaneIdRegistry::new();
        let id = reg.mint();
        assert_eq!(Ok(()), reg.take(&id));
        assert_eq!(Err(Refusal::PaneTaken), reg.take(&id));
        reg.expire(&id);
        assert_eq!(Err(Refusal::PaneTaken), reg.take(&id));
    }

    #[test]
    fn an_expired_id_is_refused() {
        let mut reg = PaneIdRegistry::new();
        let id = reg.mint();
        reg.expire(&id);
        assert_eq!(Err(Refusal::PaneExpired), reg.take(&id));
    }

    #[test]
    fn an_unminted_id_is_unknown() {
        let mut reg = PaneIdRegistry::new();
        let id = reg.mint();
        let never = PaneId::parse("2-aaaaaaaaaa").unwrap();
        assert_eq!(Err(Refusal::PaneUnknown), reg.take(&never));
        let mut forged = id;
        forged.token[0] = if id.token[0] == b'a' { b'b' } else { b'a' };
        assert_eq!(Err(Refusal::PaneUnknown), reg.take(&forged));
        // expiring a forged id leaves the real one alone
        reg.expire(&forged);
        assert_eq!(Ok(()), reg.take(&id));
    }
}
