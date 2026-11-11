//! Datagram Delivery Protocol (DDP) packet format
//!
//! See Inside AppleTalk, 2nd edition, chapter 4.

use anyhow::{Result, bail};

/// Maximum length of the data part of a DDP datagram
pub const MAX_DATA_LEN: usize = 586;

/// Length of the short DDP header
pub const SHORT_HEADER_LEN: usize = 5;

/// Length of the extended DDP header
pub const EXTENDED_HEADER_LEN: usize = 13;

/// Short DDP header, used on nonextended networks when source and
/// destination are on the same network
pub struct DdpShortHeader {
    pub dst_socket: u8,
    pub src_socket: u8,
    pub ddp_type: u8,
}

/// Extended DDP header, carries the full internet socket addresses
pub struct DdpExtendedHeader {
    /// 16-bit DDP checksum, 0 when not computed
    pub checksum: u16,
    pub dst_net: u16,
    pub src_net: u16,
    pub dst_node: u8,
    pub src_node: u8,
    pub dst_socket: u8,
    pub src_socket: u8,
    pub ddp_type: u8,
    /// Number of routers the datagram has passed through
    pub hop_count: u8,
}

/// Compute the DDP checksum of a byte stream
pub fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for &b in bytes {
        sum = sum.wrapping_add(u16::from(b)).rotate_left(1);
    }
    if sum == 0 { 0xFFFF } else { sum }
}

/// Parse a datagram with a short DDP header, returning the header and the
/// data part
pub fn parse_short(datagram: &[u8]) -> Result<(DdpShortHeader, &[u8])> {
    if datagram.len() < SHORT_HEADER_LEN {
        bail!("Short DDP datagram too small ({} bytes)", datagram.len());
    }
    let len = ((datagram[0] as usize & 0x03) << 8) | datagram[1] as usize;
    if len != datagram.len() {
        bail!(
            "Short DDP datagram length mismatch (header says {}, got {})",
            len,
            datagram.len()
        );
    }
    if len - SHORT_HEADER_LEN > MAX_DATA_LEN {
        bail!(
            "Short DDP datagram data too long ({} bytes)",
            len - SHORT_HEADER_LEN
        );
    }
    let header = DdpShortHeader {
        dst_socket: datagram[2],
        src_socket: datagram[3],
        ddp_type: datagram[4],
    };
    Ok((header, &datagram[SHORT_HEADER_LEN..]))
}

/// Build a datagram with a short DDP header
pub fn build_short(dst_socket: u8, src_socket: u8, ddp_type: u8, data: &[u8]) -> Vec<u8> {
    let len = SHORT_HEADER_LEN + data.len();
    let mut datagram = Vec::with_capacity(len);
    datagram.extend_from_slice(&(len as u16).to_be_bytes());
    datagram.extend_from_slice(&[dst_socket, src_socket, ddp_type]);
    datagram.extend_from_slice(data);
    datagram
}

/// Parse a datagram with an extended DDP header, returning the header and
/// the data part. Verifies the checksum when one is present.
pub fn parse_extended(datagram: &[u8]) -> Result<(DdpExtendedHeader, &[u8])> {
    if datagram.len() < EXTENDED_HEADER_LEN {
        bail!("Extended DDP datagram too small ({} bytes)", datagram.len());
    }
    let len = ((datagram[0] as usize & 0x03) << 8) | datagram[1] as usize;
    if len != datagram.len() {
        bail!(
            "Extended DDP datagram length mismatch (header says {}, got {})",
            len,
            datagram.len()
        );
    }
    if len - EXTENDED_HEADER_LEN > MAX_DATA_LEN {
        bail!(
            "Extended DDP datagram data too long ({} bytes)",
            len - EXTENDED_HEADER_LEN
        );
    }
    let header = DdpExtendedHeader {
        hop_count: (datagram[0] >> 2) & 0x0F,
        checksum: u16::from_be_bytes([datagram[2], datagram[3]]),
        dst_net: u16::from_be_bytes([datagram[4], datagram[5]]),
        src_net: u16::from_be_bytes([datagram[6], datagram[7]]),
        dst_node: datagram[8],
        src_node: datagram[9],
        dst_socket: datagram[10],
        src_socket: datagram[11],
        ddp_type: datagram[12],
    };
    if header.checksum != 0 && checksum(&datagram[4..]) != header.checksum {
        bail!("Extended DDP datagram checksum mismatch");
    }
    Ok((header, &datagram[EXTENDED_HEADER_LEN..]))
}

/// Build a datagram with an extended DDP header
pub fn build_extended(header: &DdpExtendedHeader, data: &[u8]) -> Vec<u8> {
    let len = EXTENDED_HEADER_LEN + data.len();
    let mut datagram = Vec::with_capacity(len);
    datagram.push((header.hop_count << 2) | ((len >> 8) as u8 & 0x03));
    datagram.push(len as u8);
    datagram.extend_from_slice(&header.checksum.to_be_bytes());
    datagram.extend_from_slice(&header.dst_net.to_be_bytes());
    datagram.extend_from_slice(&header.src_net.to_be_bytes());
    datagram.extend_from_slice(&[header.dst_node, header.src_node]);
    datagram.extend_from_slice(&[header.dst_socket, header.src_socket, header.ddp_type]);
    datagram.extend_from_slice(data);
    datagram
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum() {
        assert_eq!(checksum(&[]), 0xFFFF);
        assert_eq!(checksum(&[0x00, 0x00]), 0xFFFF);
        assert_eq!(checksum(&[0x00, 0x01]), 2);
        assert_eq!(checksum(&[0xFF, 0xFF]), 1530);
    }

    #[test]
    fn test_short_round_trip() {
        let datagram = build_short(0x42, 0x24, 3, &[1, 2, 3, 4]);
        let (header, data) = parse_short(&datagram).unwrap();
        assert_eq!(header.dst_socket, 0x42);
        assert_eq!(header.src_socket, 0x24);
        assert_eq!(header.ddp_type, 3);
        assert_eq!(data, [1, 2, 3, 4]);
    }

    #[test]
    fn test_short_bad_length() {
        let mut datagram = build_short(1, 2, 3, &[1, 2, 3]);
        datagram[1] = 99;
        assert!(parse_short(&datagram).is_err());
    }

    #[test]
    fn test_short_too_small() {
        assert!(parse_short(&[0, 5, 1, 2]).is_err());
    }

    #[test]
    fn test_extended_round_trip() {
        let header = DdpExtendedHeader {
            checksum: 0,
            dst_net: 0x1234,
            src_net: 0x5678,
            dst_node: 0x42,
            src_node: 0x24,
            dst_socket: 0x80,
            src_socket: 0x81,
            ddp_type: 3,
            hop_count: 0,
        };
        let datagram = build_extended(&header, &[1, 2, 3]);
        let (parsed, data) = parse_extended(&datagram).unwrap();
        assert_eq!(parsed.dst_net, 0x1234);
        assert_eq!(parsed.src_net, 0x5678);
        assert_eq!(parsed.dst_node, 0x42);
        assert_eq!(parsed.src_node, 0x24);
        assert_eq!(parsed.dst_socket, 0x80);
        assert_eq!(parsed.src_socket, 0x81);
        assert_eq!(parsed.ddp_type, 3);
        assert_eq!(parsed.hop_count, 0);
        assert_eq!(data, [1, 2, 3]);
    }

    #[test]
    fn test_extended_checksum() {
        let mut header = DdpExtendedHeader {
            checksum: 0,
            dst_net: 0x1234,
            src_net: 0x5678,
            dst_node: 0x42,
            src_node: 0x24,
            dst_socket: 0x80,
            src_socket: 0x81,
            ddp_type: 3,
            hop_count: 7,
        };
        let datagram = build_extended(&header, &[1, 2, 3]);
        header.checksum = checksum(&datagram[4..]);
        let datagram = build_extended(&header, &[1, 2, 3]);
        let (parsed, _) = parse_extended(&datagram).unwrap();
        assert_eq!(parsed.hop_count, 7);
        assert_eq!(parsed.checksum, header.checksum);

        // Tampering with the data must fail the checksum
        let mut bad = datagram;
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(parse_extended(&bad).is_err());
    }

    #[test]
    fn test_extended_bad_length() {
        let header = DdpExtendedHeader {
            checksum: 0,
            dst_net: 0,
            src_net: 0,
            dst_node: 0,
            src_node: 0,
            dst_socket: 1,
            src_socket: 2,
            ddp_type: 3,
            hop_count: 0,
        };
        let mut datagram = build_extended(&header, &[]);
        datagram[1] = 99;
        assert!(parse_extended(&datagram).is_err());
    }
}
