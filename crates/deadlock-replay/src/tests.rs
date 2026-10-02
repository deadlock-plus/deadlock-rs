use prost::Message;
use valveprotos::deadlock::{CMsgMatchMetaData, CMsgMatchMetaDataContents};

use crate::error::Error;
use crate::meta;

/// A `CMsgMatchMetaData` as the CDN serves it: version 1, match 42,000,000, and a contents
/// block holding one 1,800 s match with one player.
const FIXTURE_PLAIN: &[u8] = &[
    0x08, 0x01, 0x12, 0x2e, 0x12, 0x2c, 0x08, 0x88, 0x0e, 0x22, 0x22, 0x08, 0x95, 0x9a, 0xef, 0x3a,
    0x10, 0x03, 0x2a, 0x13, 0x80, 0x02, 0xf4, 0x03, 0x88, 0x02, 0xfa, 0x01, 0xb0, 0x02, 0xb0, 0x09,
    0xd0, 0x02, 0xa8, 0x46, 0x98, 0x03, 0x04, 0x30, 0x01, 0x40, 0x07, 0x60, 0x0f, 0x30, 0x80, 0xbd,
    0x83, 0x14, 0x18, 0x80, 0xbd, 0x83, 0x14,
];

/// [`FIXTURE_PLAIN`] under bzip2, standing in for a `.meta.bz2` off the replay CDN.
///
/// `bzip2-rs` only decompresses, so a compressed fixture cannot be produced in-process and
/// has to be a literal.
#[cfg(feature = "bzip2")]
const FIXTURE_BZ2: &[u8] = &[
    0x42, 0x5a, 0x68, 0x39, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0xa6, 0xc6, 0xc1, 0xa8, 0x00, 0x00,
    0x06, 0x7d, 0x7b, 0xbc, 0xe1, 0xdc, 0x40, 0x10, 0x15, 0x40, 0x10, 0x41, 0x00, 0x40, 0x00, 0x48,
    0x40, 0x02, 0x50, 0x00, 0x40, 0x40, 0x02, 0x40, 0x00, 0x00, 0x00, 0x84, 0x10, 0x20, 0x00, 0x50,
    0xc3, 0x04, 0xc0, 0x98, 0x08, 0x68, 0xc9, 0xa6, 0x04, 0x50, 0x03, 0x10, 0x1a, 0x06, 0x4f, 0x29,
    0xe9, 0x3d, 0x35, 0x2d, 0x7a, 0xa7, 0xde, 0x11, 0xea, 0xb8, 0x8f, 0x31, 0x83, 0x59, 0xe9, 0x13,
    0xc8, 0xc1, 0xac, 0x8e, 0x42, 0x10, 0x15, 0x6c, 0xc8, 0xe4, 0x4a, 0x0e, 0x30, 0x49, 0xa3, 0xa9,
    0x90, 0x4d, 0x6b, 0x0f, 0xc5, 0xdc, 0x91, 0x4e, 0x14, 0x24, 0x29, 0xb1, 0xb0, 0x6a, 0x00,
];

#[test]
fn the_cdn_payload_is_a_wrapper_whose_match_details_hold_the_contents() {
    let wrapper = meta::parse(FIXTURE_PLAIN).unwrap();
    assert_eq!(wrapper.version, Some(1));
    assert_eq!(wrapper.match_id, Some(42_000_000));

    let info = meta::contents(&wrapper).unwrap().match_info.unwrap();
    assert_eq!(info.duration_s, Some(1_800));
    assert_eq!(info.match_id, Some(42_000_000));
    assert_eq!(info.players.len(), 1);
    assert_eq!(info.players[0].stats[0].shots_hit, Some(500));
}

#[test]
fn a_truncated_message_is_rejected_rather_than_decoded_short() {
    let err = meta::parse(&FIXTURE_PLAIN[..20]).unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "{err:?}");
}

#[test]
fn metadata_with_no_match_details_reports_the_missing_block() {
    let wrapper = CMsgMatchMetaData {
        version: Some(1),
        ..CMsgMatchMetaData::default()
    };
    let err = meta::contents(&wrapper).unwrap_err();
    assert!(matches!(err, Error::MissingMatchDetails), "{err:?}");
}

#[test]
fn a_contents_block_that_is_not_a_message_is_rejected() {
    let wrapper = CMsgMatchMetaData {
        match_details: Some(vec![0x12, 0xff]),
        ..CMsgMatchMetaData::default()
    };
    let err = meta::contents(&wrapper).unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "{err:?}");
}

#[test]
fn contents_round_trip_through_the_wrapper() {
    let inner = CMsgMatchMetaDataContents::default().encode_to_vec();
    let wrapper = CMsgMatchMetaData {
        match_details: Some(inner),
        ..CMsgMatchMetaData::default()
    };
    let again = meta::parse(&wrapper.encode_to_vec()).unwrap();
    assert!(meta::contents(&again).unwrap().match_info.is_none());
}

#[cfg(feature = "bzip2")]
mod bzip2 {
    use super::*;
    use crate::{decompress, decompress_capped};

    #[test]
    fn a_bzip2_stream_decompresses_to_the_bytes_it_was_built_from() {
        assert_eq!(decompress(FIXTURE_BZ2).unwrap(), FIXTURE_PLAIN);
    }

    #[test]
    fn a_compressed_payload_decodes_end_to_end_into_a_wrapper() {
        let wrapper = meta::from_bz2(FIXTURE_BZ2).unwrap();
        assert_eq!(wrapper.match_id, Some(42_000_000));
        let info = meta::contents(&wrapper).unwrap().match_info.unwrap();
        assert_eq!(info.players[0].stats[0].shots_hit, Some(500));
    }

    #[test]
    fn a_payload_that_is_not_bzip2_is_rejected_before_any_block_is_decoded() {
        let err = decompress(b"<html>404 Not Found</html>").unwrap_err();
        assert!(matches!(err, Error::NotBzip2), "{err:?}");
    }

    #[test]
    fn a_bzip2_header_with_an_out_of_range_block_size_is_rejected() {
        let mut bad = FIXTURE_BZ2.to_vec();
        bad[3] = b'0';
        let err = decompress(&bad).unwrap_err();
        assert!(matches!(err, Error::NotBzip2), "{err:?}");
    }

    #[test]
    fn a_stream_cut_short_is_rejected_rather_than_yielding_a_partial_message() {
        let err = decompress(&FIXTURE_BZ2[..FIXTURE_BZ2.len() / 2]).unwrap_err();
        assert!(
            matches!(err, Error::Bzip2(_) | Error::TruncatedStream),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_payload_is_rejected_rather_than_spinning() {
        let err = decompress(&[]).unwrap_err();
        assert!(matches!(err, Error::NotBzip2), "{err:?}");
    }

    #[test]
    fn decompression_stops_at_the_byte_cap_instead_of_expanding_a_bomb() {
        let err = decompress_capped(FIXTURE_BZ2, 8).unwrap_err();
        assert!(matches!(err, Error::TooLarge { cap: 8 }), "{err:?}");
    }

    #[test]
    fn a_cap_at_exactly_the_decompressed_length_is_not_exceeded() {
        assert_eq!(
            decompress_capped(FIXTURE_BZ2, FIXTURE_PLAIN.len()).unwrap(),
            FIXTURE_PLAIN
        );
    }
}
