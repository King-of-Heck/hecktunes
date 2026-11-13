//! MPEG audio frame headers (ISO 11172-3 / 13818-3), as much as cutting Layer III needs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

/// Layer III bitrates in kbit/s by index (0 = free format, 15 = invalid).
const L3_MPEG1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const L3_MPEG2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const RATES_MPEG1: [u32; 3] = [44100, 48000, 32000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: Version,
    pub layer: u8,
    /// 0 = free format.
    pub bitrate_index: u8,
    pub sample_rate: u32,
    pub padding: bool,
    /// Channel mode 0b11.
    pub mono: bool,
}

/// Parses a 4-byte frame header; None if it isn't one (no sync, reserved version/layer,
/// bitrate index 15, sample-rate index 3).
pub fn parse(b: [u8; 4]) -> Option<Header> {
    if b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (b[1] >> 3) & 0b11 {
        0b00 => Version::Mpeg25,
        0b10 => Version::Mpeg2,
        0b11 => Version::Mpeg1,
        _ => return None,
    };
    let layer = match (b[1] >> 1) & 0b11 {
        0b01 => 3,
        0b10 => 2,
        0b11 => 1,
        _ => return None,
    };
    let bitrate_index = b[2] >> 4;
    let rate_index = ((b[2] >> 2) & 0b11) as usize;
    if bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let base = RATES_MPEG1[rate_index];
    let sample_rate = match version {
        Version::Mpeg1 => base,
        Version::Mpeg2 => base / 2,
        Version::Mpeg25 => base / 4,
    };
    Some(Header { version, layer, bitrate_index, sample_rate, padding: b[2] & 0b10 != 0, mono: b[3] >> 6 == 0b11 })
}

/// Frame length in bytes for a Layer III header field set.
pub fn layer3_frame_len(version: Version, bitrate_index: u8, sample_rate: u32, padding: bool) -> u32 {
    let (table, coef) = match version {
        Version::Mpeg1 => (&L3_MPEG1, 144_000),
        _ => (&L3_MPEG2, 72_000),
    };
    coef * table[bitrate_index as usize] / sample_rate + padding as u32
}

impl Header {
    /// Layer III: 1152 for MPEG-1, 576 for MPEG-2/2.5.
    pub fn samples_per_frame(&self) -> u32 {
        if self.version == Version::Mpeg1 { 1152 } else { 576 }
    }
    /// Layer III bitrate; 0 for free format.
    pub fn bitrate_kbps(&self) -> u32 {
        let table = if self.version == Version::Mpeg1 { &L3_MPEG1 } else { &L3_MPEG2 };
        table[self.bitrate_index as usize]
    }
    /// Bytes including the header; Layer III only.
    pub fn frame_len(&self) -> u32 {
        layer3_frame_len(self.version, self.bitrate_index, self.sample_rate, self.padding)
    }
    pub fn side_info_len(&self) -> usize {
        match (self.version == Version::Mpeg1, self.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        }
    }
    pub fn channels(&self) -> u16 {
        if self.mono { 1 } else { 2 }
    }
    /// Same stream: version, layer, sample rate and mono/stereo match.
    pub fn same_stream(&self, o: &Header) -> bool {
        (self.version, self.layer, self.sample_rate, self.mono) == (o.version, o.layer, o.sample_rate, o.mono)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mpeg1_layer3_128k() {
        let h = parse([0xFF, 0xFB, 0x90, 0x00]).unwrap();
        assert_eq!(h.version, Version::Mpeg1);
        assert_eq!(h.layer, 3);
        assert_eq!(h.bitrate_index, 9);
        assert_eq!(h.bitrate_kbps(), 128);
        assert_eq!(h.sample_rate, 44100);
        assert!(!h.padding);
        assert!(!h.mono);
        assert_eq!(h.frame_len(), 417);
        assert_eq!(h.samples_per_frame(), 1152);
        assert_eq!(h.side_info_len(), 32);
        assert_eq!(h.channels(), 2);
    }

    #[test]
    fn parses_mpeg2_mono() {
        let h = parse([0xFF, 0xF3, 0x60, 0xC0]).unwrap();
        assert_eq!(h.version, Version::Mpeg2);
        assert_eq!(h.layer, 3);
        assert_eq!(h.bitrate_index, 6);
        assert_eq!(h.bitrate_kbps(), 48);
        assert_eq!(h.sample_rate, 22050);
        assert!(h.mono);
        assert_eq!(h.frame_len(), 156);
        assert_eq!(h.samples_per_frame(), 576);
        assert_eq!(h.side_info_len(), 9);
        assert_eq!(h.channels(), 1);
    }

    #[test]
    fn parses_layer2() {
        assert_eq!(parse([0xFF, 0xFD, 0x90, 0x00]).unwrap().layer, 2);
    }

    #[test]
    fn rejects_invalid_headers() {
        assert!(parse([0, 0, 0, 0]).is_none());
        assert!(parse([0xFF, 0xFB, 0xF0, 0]).is_none());
        assert!(parse([0xFF, 0xFB, 0x9C, 0]).is_none());
        assert!(parse([0xFF, 0xEB, 0x90, 0]).is_none());
    }

    #[test]
    fn tiny_mpeg2_frame_is_too_small_for_xing() {
        assert_eq!(layer3_frame_len(Version::Mpeg2, 1, 22050, false), 26);
    }
}
