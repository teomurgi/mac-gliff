//! Annex B byte-stream helpers for feeding VideoToolbox: split NAL units,
//! pull out the parameter sets, and frame an access unit as AVCC.
//!
//! The NAL splitting is adapted from `gliff-va`'s `h264::annexb`
//! (omacom/gliff, MIT). The gliff protocol delivers one complete access
//! unit per `VideoFrame` payload — SPS and PPS in front of every IDR — so
//! no access-unit assembly is needed here.

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

/// One NAL unit: its type, `nal_ref_idc`, and its bytes starting at the
/// header byte (no start code).
#[derive(Debug, Clone, Copy)]
pub struct Nal<'a> {
    pub nal_type: u8,
    pub ref_idc: u8,
    pub data: &'a [u8],
}

/// Split an Annex B stream at its start codes.
pub fn nal_units(data: &[u8]) -> Vec<Nal<'_>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(n, &body)| {
            // Trailing zero bytes are never part of a NAL unit (the RBSP stop
            // bit and cabac_zero_words both end in a nonzero byte): they are
            // the leading zero of a 4-byte start code or trailing_zero_8bits.
            let mut end = starts.get(n + 1).map(|s| s - 3).unwrap_or(data.len());
            while end > body && data[end - 1] == 0 {
                end -= 1;
            }
            let unit = &data[body..end];
            unit.first().map(|h| Nal {
                nal_type: h & 0x1f,
                ref_idc: (h >> 5) & 3,
                data: unit,
            })
        })
        .collect()
}

/// The SPS and PPS units of a stream, with their NAL header bytes, as
/// `CMVideoFormatDescriptionCreateFromH264ParameterSets` wants them.
/// `None` when either kind is missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParameterSets<'a> {
    pub sps: Vec<&'a [u8]>,
    pub pps: Vec<&'a [u8]>,
}

pub fn parameter_sets(data: &[u8]) -> Option<ParameterSets<'_>> {
    let mut ps = ParameterSets::default();
    for n in nal_units(data) {
        match n.nal_type {
            NAL_SPS => ps.sps.push(n.data),
            NAL_PPS => ps.pps.push(n.data),
            _ => {}
        }
    }
    if ps.sps.is_empty() || ps.pps.is_empty() {
        return None;
    }
    Some(ps)
}

/// Frame the coded units of an access unit as AVCC: a 4-byte big-endian
/// length before every NAL, start codes stripped, parameter sets and AUD
/// removed (they live in the format description). Emulation prevention
/// bytes are left alone: VideoToolbox removes them itself.
pub fn avcc(au: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len());
    for n in nal_units(au) {
        if matches!(n.nal_type, NAL_SPS | NAL_PPS | NAL_AUD) {
            continue;
        }
        out.extend_from_slice(&(n.data.len() as u32).to_be_bytes());
        out.extend_from_slice(n.data);
    }
    out
}

/// Split a byte stream into access units: a new one starts at each slice
/// NAL that follows a slice. The gliff protocol never needs this (each
/// `VideoFrame` payload is already one unit); it exists for recorded
/// streams and test fixtures.
pub fn access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut au_has_slice = false;
    for n in nal_units(data) {
        let is_slice = matches!(n.nal_type, NAL_IDR | NAL_SLICE);
        if is_slice && au_has_slice {
            aus.push(Vec::new());
            au_has_slice = false;
        }
        if aus.is_empty() {
            aus.push(Vec::new());
        }
        let au = aus.last_mut().expect("just pushed");
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(n.data);
        au_has_slice |= is_slice;
    }
    aus
}

#[cfg(test)]
mod tests {
    use super::*;

    /// x264 High profile, 64x64, two frames (IDR + P), CABAC, 8x8
    /// transform; shared with gliff-va's parser tests.
    const STREAM: &[u8] = include_bytes!("../../gliff-va/src/h264/testdata/x264-64x64.264");

    #[test]
    fn splits_units() {
        let s = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9,
        ];
        let units = nal_units(&s);
        let v: Vec<(u8, u8, usize)> = units
            .iter()
            .map(|n| (n.nal_type, n.ref_idc, n.data.len()))
            .collect();
        assert_eq!(v, vec![(NAL_SPS, 3, 3), (NAL_PPS, 3, 2), (NAL_IDR, 3, 3)]);
    }

    #[test]
    fn fixture_units() {
        let units = nal_units(STREAM);
        assert_eq!(units.first().map(|n| n.nal_type), Some(NAL_SPS));
        let types: Vec<u8> = units.iter().map(|n| n.nal_type).collect();
        assert!(types.contains(&NAL_PPS));
        assert!(types.contains(&NAL_IDR));
        assert_eq!(types.iter().filter(|&&t| t == NAL_SLICE).count(), 1);
        for n in &units {
            assert!(!n.data.is_empty());
            assert_eq!(n.nal_type, n.data[0] & 0x1f);
        }
    }

    #[test]
    fn fixture_parameter_sets() {
        let ps = parameter_sets(STREAM).expect("sps and pps");
        assert_eq!(ps.sps.len(), 1);
        assert_eq!(ps.pps.len(), 1);
        assert_eq!(ps.sps[0][0] & 0x1f, NAL_SPS);
        assert_eq!(ps.pps[0][0] & 0x1f, NAL_PPS);
        // High profile, level 10, matching gliff-va's parser test.
        assert_eq!(ps.sps[0][1], 100);
        assert_eq!(ps.sps[0][3], 10);
    }

    #[test]
    fn avcc_frames_and_skips_config() {
        let out = avcc(STREAM);
        let expect: usize = nal_units(STREAM)
            .iter()
            .filter(|n| ![NAL_SPS, NAL_PPS, NAL_AUD].contains(&n.nal_type))
            .map(|n| 4 + n.data.len())
            .sum();
        assert_eq!(out.len(), expect);
        let mut r = &out[..];
        while !r.is_empty() {
            let len = u32::from_be_bytes([r[0], r[1], r[2], r[3]]) as usize;
            assert!(len >= 2 && len <= r.len() - 4, "bad nal length {len}");
            r = &r[4 + len..];
        }
    }

    #[test]
    fn missing_parameter_sets_is_none() {
        assert!(parameter_sets(&[0, 0, 1, 0x65, 1]).is_none());
    }
}
