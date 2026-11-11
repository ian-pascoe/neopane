//! Rex ids.

/// `block:` and a UUID.
pub(crate) type Block = [u8; 42];

/// A malformed Rex id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    BadRexBlock,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::BadRexBlock => "BadRexBlock",
        })
    }
}

impl std::error::Error for Error {}

pub(crate) fn block(id: &[u8]) -> Result<Block, Error> {
    id.try_into().map_err(|_| Error::BadRexBlock)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ID: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

    #[test]
    fn block_takes_42_byte_ids_only() {
        let b = block(TEST_ID.as_bytes()).unwrap();
        assert_eq!(TEST_ID.as_bytes(), &b);
        assert_eq!(Err(Error::BadRexBlock), block(&TEST_ID.as_bytes()[..41]));
        assert_eq!(
            Err(Error::BadRexBlock),
            block(format!("{TEST_ID}0").as_bytes())
        );
        assert_eq!(Err(Error::BadRexBlock), block(b""));
    }
}
