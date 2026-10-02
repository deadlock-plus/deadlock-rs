//! `.meta.bz2` match metadata: the CDN's wrapper and the contents it carries.
//!
//! The message types are `valveprotos`'. `CMsgMatchMetaData` is a wrapper whose
//! `match_details` is declared `bytes`, so the match itself is a second, separately
//! serialised `CMsgMatchMetaDataContents`; [`contents`] decodes it.

use prost::Message;
use valveprotos::deadlock::{CMsgMatchMetaData, CMsgMatchMetaDataContents};

use crate::error::{Error, Result};

/// Decode the wrapper from decompressed bytes.
///
/// # Errors
///
/// If the bytes are not a decodable message.
pub fn parse(bytes: &[u8]) -> Result<CMsgMatchMetaData> {
    Ok(CMsgMatchMetaData::decode(bytes)?)
}

/// Decompress a `.meta.bz2` payload and decode the wrapper inside it.
///
/// Takes the bytes rather than a URL: fetching is the caller's problem.
///
/// # Errors
///
/// If the payload is not a bzip2 stream, does not decompress, exceeds
/// [`DEFAULT_MAX_DECOMPRESSED`], or does not decode.
///
/// [`DEFAULT_MAX_DECOMPRESSED`]: crate::DEFAULT_MAX_DECOMPRESSED
#[cfg(feature = "bzip2")]
pub fn from_bz2(payload: &[u8]) -> Result<CMsgMatchMetaData> {
    parse(&crate::bz2::decompress(payload)?)
}

/// Decode the `match_details` block of a wrapper.
///
/// # Errors
///
/// [`Error::MissingMatchDetails`] if the wrapper carries none, or [`Error::Decode`] if the
/// block is not a message.
pub fn contents(meta: &CMsgMatchMetaData) -> Result<CMsgMatchMetaDataContents> {
    let bytes = meta
        .match_details
        .as_deref()
        .ok_or(Error::MissingMatchDetails)?;
    Ok(CMsgMatchMetaDataContents::decode(bytes)?)
}
