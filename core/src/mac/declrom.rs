//! Declaration ROMs built at run time, for cards whose ROM holds no driver:
//! only the structures the Slot Manager reads (Designing Cards and Drivers
//! for the Macintosh Family, third edition, chapter 8).
//!
//! The image is the content of the ROM chip, from its lowest address up: the
//! card puts it on the byte lanes that its ByteLanes field gives.

/// Ids of sResource list entries
pub mod id {
    pub const RSRC_TYPE: u8 = 0x01;
    pub const RSRC_NAME: u8 = 0x02;
    pub const RSRC_FLAGS: u8 = 0x07;
    pub const RSRC_HW_DEV_ID: u8 = 0x08;
    pub const BOARD_ID: u8 = 0x20;
    pub const PRIMARY_INIT: u8 = 0x22;
    pub const VENDOR_INFO: u8 = 0x24;
    pub const SECONDARY_INIT: u8 = 0x26;

    /// Ids of a VendorInfo list
    pub const VENDOR_ID: u8 = 0x01;
    pub const REV_LEVEL: u8 = 0x03;
    pub const PART_NUM: u8 = 0x04;

    /// Id of the board sResource in the sResource directory. The card's other
    /// sResources have ids from $80 up.
    pub const BOARD: u8 = 0x01;
}

/// sResource category and type of the board sResource
pub const CAT_BOARD: u16 = 0x0001;
/// sResource category and type of an Ethernet interface
pub const CAT_NETWORK: u16 = 0x0004;
pub const TYPE_ETHERNET: u16 = 0x0001;

/// TestPattern of the format block
const TEST_PATTERN: u32 = 0x5A93_2BC7;
/// Format field of the format block: 1, Apple's format
const FORMAT_APPLE: u8 = 1;
/// Size of the format block, at the top of the ROM
const FORMAT_BLOCK: usize = 20;

/// One entry of an sResource list
pub enum Entry {
    /// A value held in the entry itself (24 bits)
    Value(u8, u32),
    /// Data elsewhere in the ROM, which the entry points to
    Data(u8, Vec<u8>),
    /// A list elsewhere in the ROM, which the entry points to
    List(u8, Vec<Self>),
}

impl Entry {
    /// sRsrcType: category, type, driver software and hardware ids
    pub fn rsrc_type(category: u16, ctype: u16, drvr_sw: u16, drvr_hw: u16) -> Self {
        let data = [category, ctype, drvr_sw, drvr_hw]
            .iter()
            .flat_map(|w| w.to_be_bytes())
            .collect();
        Self::Data(id::RSRC_TYPE, data)
    }

    /// A C string
    pub fn string(id: u8, s: &str) -> Self {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        Self::Data(id, data)
    }

    /// An sExec block of 68020 code that sets seStatus to `status`, in the
    /// SEBlock that A0 points to, then returns
    pub fn exec_block(id: u8, status: u16) -> Self {
        let mut code = vec![0x31, 0x7C]; // move.w #status,2(a0)
        code.extend(status.to_be_bytes());
        code.extend([0x00, 0x02, 0x4E, 0x75]); // rts
        let len = (4 + 1 + 1 + 2 + 4 + code.len()) as u32;
        let mut data = len.to_be_bytes().to_vec();
        // Header: revision 2, CPU 68020, a reserved word, then the offset of the
        // code from this offset field
        data.extend([0x02, 0x02, 0x00, 0x00]);
        data.extend(4_u32.to_be_bytes());
        data.extend(code);
        Self::Data(id, data)
    }
}

/// An sResource of the directory
pub struct SResource {
    pub id: u8,
    pub entries: Vec<Entry>,
}

/// A declaration ROM: the directory and the format block
pub struct DeclRom {
    pub resources: Vec<SResource>,
    pub rev_level: u8,
    /// The byte lanes the ROM chip is on, as the low nibble of the ByteLanes
    /// field
    pub byte_lanes: u8,
}

/// The ROM being laid out: its bytes so far, and the entries whose offsets
/// can only be set once what they point to is placed
struct Layout {
    rom: Vec<u8>,
    /// Entry to patch: its address, its id, and what it points to
    pending: Vec<(usize, u8, Pending)>,
}

enum Pending {
    Data(Vec<u8>),
    List(Vec<Entry>),
}

impl Layout {
    fn put_long(&mut self, at: usize, val: u32) {
        self.rom[at..at + 4].copy_from_slice(&val.to_be_bytes());
    }

    /// Appends a list of entries, with its end-of-list entry, to the ROM, and
    /// returns its offset
    fn add_list(&mut self, entries: Vec<Entry>) -> usize {
        let start = self.rom.len();
        let count = entries.len();
        self.rom.resize(start + 4 * (count + 1), 0);
        for (n, entry) in entries.into_iter().enumerate() {
            let at = start + 4 * n;
            match entry {
                Entry::Value(id, val) => {
                    assert!(val <= 0xFF_FFFF);
                    self.put_long(at, (u32::from(id) << 24) | val);
                }
                Entry::Data(id, data) => self.pending.push((at, id, Pending::Data(data))),
                Entry::List(id, list) => self.pending.push((at, id, Pending::List(list))),
            }
        }
        self.put_long(start + 4 * count, 0xFF00_0000);
        start
    }

    /// Appends the data and the lists that the entries point to, then sets the
    /// entries' offsets to them
    fn place_pending(&mut self) {
        while let Some((at, id, what)) = self.pending.pop() {
            let target = match what {
                Pending::Data(data) => {
                    let target = self.rom.len();
                    self.rom.extend(data);
                    // Align the lists that follow on long words
                    self.rom.resize(self.rom.len().next_multiple_of(4), 0);
                    target
                }
                Pending::List(list) => self.add_list(list),
            };
            let offset = (target - at) as u32;
            assert!(offset <= 0x7F_FFFF);
            self.put_long(at, (u32::from(id) << 24) | offset);
        }
    }
}

impl DeclRom {
    /// The ROM image
    pub fn build(self) -> Vec<u8> {
        let mut layout = Layout {
            rom: vec![],
            pending: vec![],
        };
        let directory = self
            .resources
            .into_iter()
            .map(|r| Entry::List(r.id, r.entries))
            .collect();
        let dir = layout.add_list(directory);
        layout.place_pending();

        let mut rom = layout.rom;
        let fb = rom.len();
        rom.resize(fb + FORMAT_BLOCK, 0);
        let dir_offset = (dir as i64 - fb as i64) as u32 & 0xFF_FFFF;
        rom[fb..fb + 4].copy_from_slice(&dir_offset.to_be_bytes());
        let len = rom.len() as u32;
        rom[fb + 4..fb + 8].copy_from_slice(&len.to_be_bytes());
        rom[fb + 12] = self.rev_level;
        rom[fb + 13] = FORMAT_APPLE;
        rom[fb + 14..fb + 18].copy_from_slice(&TEST_PATTERN.to_be_bytes());
        rom[fb + 19] = (self.byte_lanes & 0x0F) | (!self.byte_lanes << 4);
        let crc = checksum(&rom);
        rom[fb + 8..fb + 12].copy_from_slice(&crc.to_be_bytes());
        rom
    }
}

/// The checksum of a ROM image: for each byte in ascending address order,
/// rotate the sum left by one bit, then add the byte. The CRC field of the
/// format block counts as zeroes.
pub fn checksum(rom: &[u8]) -> u32 {
    let crc_field = rom.len() - FORMAT_BLOCK + 8;
    rom.iter().enumerate().fold(0_u32, |sum, (i, &b)| {
        let b = if (crc_field..crc_field + 4).contains(&i) {
            0
        } else {
            b
        };
        sum.rotate_left(1).wrapping_add(u32::from(b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(rom: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(rom[at..at + 4].try_into().unwrap())
    }

    /// Where an entry points
    fn target(at: usize, entry: u32) -> usize {
        let offset = ((entry << 8) as i32) >> 8;
        (at as i64 + i64::from(offset)) as usize
    }

    /// The entries of the list at offset `at`: for each, its id, its offset,
    /// and the whole entry long word
    fn list(rom: &[u8], at: usize) -> Vec<(u8, usize, u32)> {
        let mut entries = vec![];
        for n in 0.. {
            let entry = long(rom, at + 4 * n);
            if entry >> 24 == 0xFF {
                break;
            }
            entries.push(((entry >> 24) as u8, at + 4 * n, entry));
        }
        entries
    }

    fn cstr(rom: &[u8], at: usize) -> &str {
        let end = rom[at..].iter().position(|&b| b == 0).unwrap();
        std::str::from_utf8(&rom[at..at + end]).unwrap()
    }

    fn twisted_pair() -> Vec<u8> {
        DeclRom {
            resources: vec![
                SResource {
                    id: id::BOARD,
                    entries: vec![
                        Entry::rsrc_type(CAT_BOARD, 0, 0, 0),
                        Entry::string(id::RSRC_NAME, "Test Card"),
                        Entry::Value(id::BOARD_ID, 0x5D),
                        Entry::exec_block(id::PRIMARY_INIT, 2),
                        Entry::List(
                            id::VENDOR_INFO,
                            vec![
                                Entry::string(id::VENDOR_ID, "Vendor"),
                                Entry::string(id::PART_NUM, "000-0000"),
                            ],
                        ),
                    ],
                },
                SResource {
                    id: 0x80,
                    entries: vec![
                        Entry::rsrc_type(CAT_NETWORK, TYPE_ETHERNET, 0, 0x0119),
                        Entry::string(id::RSRC_NAME, "Network_EtherNet_none_Test"),
                        Entry::Value(id::RSRC_HW_DEV_ID, 1),
                    ],
                },
            ],
            rev_level: 1,
            byte_lanes: 0b0010,
        }
        .build()
    }

    #[test]
    fn format_block() {
        let rom = twisted_pair();
        let fb = rom.len() - FORMAT_BLOCK;
        assert_eq!(long(&rom, fb + 4) as usize, rom.len());
        assert_eq!(long(&rom, fb + 8), checksum(&rom));
        assert_eq!(rom[fb + 12], 1);
        assert_eq!(rom[fb + 13], FORMAT_APPLE);
        assert_eq!(long(&rom, fb + 14), TEST_PATTERN);
        assert_eq!(rom[fb + 19], 0xD2);
        // The directory, at the start
        assert_eq!(target(fb, long(&rom, fb)), 0);
        assert_eq!(long(&rom, fb) >> 24, 0);
    }

    #[test]
    fn sresources() {
        let rom = twisted_pair();
        let dir = list(&rom, 0);
        assert_eq!(dir.iter().map(|e| e.0).collect::<Vec<_>>(), [0x01, 0x80]);

        let board = list(&rom, target(dir[0].1, dir[0].2));
        let (_, at, entry) = board[1];
        assert_eq!(cstr(&rom, target(at, entry)), "Test Card");
        assert_eq!(board[2].2 & 0xFF_FFFF, 0x5D);
        let (_, at, entry) = board[3];
        let block = target(at, entry);
        assert_eq!(
            rom[block..block + 20],
            [
                0x00, 0x00, 0x00, 0x14, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x31, 0x7C,
                0x00, 0x02, 0x00, 0x02, 0x4E, 0x75
            ]
        );
        let (_, at, entry) = board[4];
        let vendor = list(&rom, target(at, entry));
        let (_, at, entry) = vendor[1];
        assert_eq!(cstr(&rom, target(at, entry)), "000-0000");

        let enet = list(&rom, target(dir[1].1, dir[1].2));
        let (_, at, entry) = enet[0];
        let t = target(at, entry);
        assert_eq!(rom[t..t + 8], [0, 4, 0, 1, 0, 0, 0x01, 0x19]);
        assert_eq!(enet[2], (id::RSRC_HW_DEV_ID, enet[2].1, 0x0800_0001));
    }

    #[test]
    fn checksum_skips_its_field() {
        let mut rom = twisted_pair();
        let crc = checksum(&rom);
        let fb = rom.len() - FORMAT_BLOCK;
        rom[fb + 8] ^= 0xFF;
        assert_eq!(checksum(&rom), crc);
        rom[0] ^= 0xFF;
        assert_ne!(checksum(&rom), crc);
    }
}
