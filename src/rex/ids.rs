//! Rex ids.

/// `block:` and a UUID.
pub(crate) type Block = [u8; 42];

/// A malformed Rex id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    BadRexBlock,
    BadRexLayer,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::BadRexBlock => "BadRexBlock",
            Error::BadRexLayer => "BadRexLayer",
        })
    }
}

impl std::error::Error for Error {}

pub(crate) fn block(id: &[u8]) -> Result<Block, Error> {
    id.try_into().map_err(|_| Error::BadRexBlock)
}

/// `layer:` and a UUID.
pub(crate) type Layer = [u8; 42];

pub(crate) fn layer(id: &[u8]) -> Result<Layer, Error> {
    id.try_into().map_err(|_| Error::BadRexLayer)
}

pub(crate) fn same_block(a: Block, b: Block) -> bool {
    a == b
}
