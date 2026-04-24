use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use windows::Win32::Devices::Cdrom::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::*;
use windows::Win32::System::Ioctl::*;
use windows::Win32::System::WindowsProgramming::*;
use windows::core::HSTRING;

use crate::mac::scsi::cdrom::CdromError;
use crate::mac::scsi::cdrom::DATA_TRACK;
use crate::mac::scsi::cdrom::LBA_START_SECTOR;
use crate::mac::scsi::cdrom::Msf;
use crate::mac::scsi::cdrom::RAW_SECTOR_LEN;
use crate::mac::scsi::cdrom::RawSector;
use crate::mac::scsi::cdrom::SessionInfo;
use crate::mac::scsi::cdrom::backends::PhysicalCdromDrive;
use crate::mac::scsi::cdrom::{CdromBackend, TrackInfo};

fn enum_logical_cdrom_drives() -> Result<Vec<String>> {
    // Get buffer size
    let buflen = unsafe { GetLogicalDriveStringsW(None) } as usize;

    // Get logical drives as a sequence of null-terminated names followed by an extra null terminator
    let mut logical_drives: Vec<u16> = vec![0; buflen];
    let buflen = unsafe { GetLogicalDriveStringsW(Some(logical_drives.as_mut_slice())) } as usize;
    if buflen == 0 {
        bail!("Failed to query logical drives");
    }

    logical_drives.resize(buflen - 1, 0); // Subtract 1 to omit the last null terminator
    let logical_drives = logical_drives
        .split(|c| *c == 0)
        .filter(|s| unsafe { GetDriveTypeW(&HSTRING::from_wide(s)) } == DRIVE_CDROM)
        .map(String::from_utf16_lossy)
        .collect();
    Ok(logical_drives)
}

pub fn query_physical_cdrom_drives() -> Vec<PhysicalCdromDrive> {
    if let Ok(drives) = enum_logical_cdrom_drives() {
        // TODO: Windows makes it weirdly difficult to get the device name behind
        // a drive letter, but it might be worth it for the friendly name.
        drives
            .iter()
            .map(|drive| PhysicalCdromDrive {
                friendly_name: drive.to_string(),
                path: format!(r"\\.\{}:", drive.chars().nth(0).unwrap()).into(),
            })
            .collect()
    } else {
        vec![]
    }
}

pub fn is_physical_cdrom_drive_path(path: &Path) -> bool {
    // CD-ROM paths are in the form "\\.\D:" where D is the drive letter.
    // If a trailing \ is added and passed to GetDriveType, it will report
    // whether a CD-ROM drive is there.
    let path = path.join(Path::new("\\"));
    unsafe { GetDriveTypeW(&HSTRING::from(path.as_path())) == DRIVE_CDROM }
}

pub struct WindowsDriveCdromBackend {
    path: PathBuf,
    handle: HANDLE,
    capacity: usize,
    sessions: Vec<SessionInfo>,
    tracks: Vec<TrackInfo>,
}

// SAFETY: The drive handle should be safe to send between threads.
unsafe impl Send for WindowsDriveCdromBackend {}

fn get_disk_length(handle: HANDLE) -> Result<i64> {
    let mut gli = GET_LENGTH_INFORMATION::default();

    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some(std::ptr::from_mut(&mut gli).cast::<c_void>()),
            std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
            None,
            None,
        )
    }?;

    Ok(gli.Length)
}

fn read_formatted_toc(handle: HANDLE) -> Result<(Vec<SessionInfo>, Vec<TrackInfo>)> {
    let mut read_toc_cmd = CDROM_READ_TOC_EX::default();
    let format = CDROM_READ_TOC_EX_FORMAT_TOC as u8;
    let msf = 1;
    // Bitfield:
    // UCHAR Format : 4;
    // UCHAR Reserved1 : 3;
    // UCHAR Msf : 1;
    // Note that in MSVC, bitfields are packed from LSB-to-MSB!
    read_toc_cmd._bitfield = (msf << 7) | format;

    const HEADER_LEN: usize = std::mem::offset_of!(CDROM_TOC, TrackData);

    let mut out_data = [0u8; std::mem::size_of::<CDROM_TOC>()];
    let mut bytes_returned = 0u32;

    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_CDROM_READ_TOC_EX,
            Some(std::ptr::from_ref(&read_toc_cmd).cast::<c_void>()),
            std::mem::size_of::<CDROM_READ_TOC_EX>() as u32,
            Some(out_data.as_mut_ptr().cast::<c_void>()),
            out_data.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }?;

    if (bytes_returned as usize) < HEADER_LEN {
        bail!("Not enough TOC bytes returned");
    }

    let toc_len =
        u16::from_be_bytes(unsafe { out_data.as_ptr().cast::<CDROM_TOC>().read() }.Length) as usize;
    let toc_data = out_data
        .get(..bytes_returned as usize)
        .ok_or_else(|| anyhow!("Invalid number of bytes returned"))?;
    let toc_data = toc_data
        .get(..toc_len + 2)
        .ok_or_else(|| anyhow!("Invalid TOC length field"))?;

    let track_data = &toc_data[HEADER_LEN..];
    let track_count = track_data.len() / std::mem::size_of::<TRACK_DATA>();

    let track_data = unsafe {
        std::slice::from_raw_parts(track_data.as_ptr().cast::<TRACK_DATA>(), track_count)
    };

    let mut leadout = LBA_START_SECTOR;
    let mut tracks = vec![];

    for data in track_data {
        let control = data._bitfield & 0xf;
        let sector = Msf::from_bytes(data.Address[1..=3].try_into().unwrap()).to_sector();

        match data.TrackNumber {
            1..=99 => {
                tracks.push(TrackInfo {
                    tno: data.TrackNumber,
                    session: 1,
                    control,
                    sector,
                });
            }
            0xAA => {
                leadout = sector;
                break;
            }
            _ => {
                log::warn!(
                    "Unhandled TNO 0x{:X} found in formatted TOC",
                    data.TrackNumber
                );
            }
        }
    }

    Ok((
        vec![SessionInfo {
            number: 1,
            disc_type: 0x00,
            leadin: 0,
            leadout,
        }],
        tracks,
    ))
}

fn read_full_toc(handle: HANDLE) -> Result<(Vec<SessionInfo>, Vec<TrackInfo>)> {
    let mut read_toc_cmd = CDROM_READ_TOC_EX::default();
    let format = CDROM_READ_TOC_EX_FORMAT_FULL_TOC as u8;
    // MSF is must be 1 when querying full TOC.
    let msf = 1;
    // Bitfield:
    // UCHAR Format : 4;
    // UCHAR Reserved1 : 3;
    // UCHAR Msf : 1;
    // Note that in MSVC, bitfields are packed from LSB-to-MSB!
    read_toc_cmd._bitfield = (msf << 7) | format;

    // Note: struct CDROM_TOC_FULL_TOC_DATA contains an array of 1 descriptor,
    // but there can actually be many descriptors following the header.
    const HEADER_LEN: usize = std::mem::offset_of!(CDROM_TOC_FULL_TOC_DATA, Descriptors);
    // Note: Passing any value greater than MAX_OUT_LEN for the output size causes
    // DeviceIoControl to throw an invalid parameter error.
    const MAX_OUT_LEN: usize =
        HEADER_LEN + 100 * std::mem::size_of::<CDROM_TOC_FULL_TOC_DATA_BLOCK>();

    let mut out_data = [0u8; MAX_OUT_LEN];
    let mut bytes_returned: u32 = 0;

    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_CDROM_READ_TOC_EX,
            Some(std::ptr::from_ref(&read_toc_cmd).cast::<c_void>()),
            std::mem::size_of::<CDROM_READ_TOC_EX>() as u32,
            Some(out_data.as_mut_ptr().cast::<c_void>()),
            out_data.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }?;

    if (bytes_returned as usize) < HEADER_LEN {
        bail!("Not enough TOC bytes returned");
    }

    let toc_len = u16::from_be_bytes(
        unsafe { out_data.as_ptr().cast::<CDROM_TOC_FULL_TOC_DATA>().read() }.Length,
    ) as usize;
    let toc_data = out_data
        .get(..bytes_returned as usize)
        .ok_or_else(|| anyhow!("Invalid number of bytes returned"))?;
    let toc_data = toc_data
        .get(0..toc_len + 2)
        .ok_or_else(|| anyhow!("Invalid Full TOC length field"))?;

    let descriptors = &toc_data[HEADER_LEN..];
    let descriptor_count = descriptors.len() / std::mem::size_of::<CDROM_TOC_FULL_TOC_DATA_BLOCK>();

    let descriptors = unsafe {
        std::slice::from_raw_parts(
            descriptors.as_ptr().cast::<CDROM_TOC_FULL_TOC_DATA_BLOCK>(),
            descriptor_count,
        )
    };

    let mut sessions = vec![];
    let mut tracks = vec![];
    let mut next_leadin = 0;

    for (i, desc) in descriptors.iter().enumerate() {
        let adr = desc._bitfield >> 4;
        let control = desc._bitfield & 0xf;
        log::debug!(
            "#{}: session {} adr {} control {} tno {} point {} msf_extra {} msf {}",
            i,
            desc.SessionNumber,
            adr,
            control,
            desc.Reserved1, // Actually the TNO field?
            desc.Point,
            Msf::from_bytes(desc.MsfExtra),
            Msf::from_bytes(desc.Msf),
        );

        if desc.SessionNumber as usize > sessions.len() {
            let leadout = sessions
                .last()
                .map(|s: &SessionInfo| s.leadout)
                .unwrap_or(LBA_START_SECTOR);
            while sessions.len() < desc.SessionNumber as usize {
                sessions.push(SessionInfo {
                    number: (sessions.len() + 1).try_into().unwrap(),
                    disc_type: 0x00,
                    leadin: next_leadin,
                    leadout,
                });
                next_leadin = leadout;
            }
        }

        // SessionNumber starts at 1. Subtract 1 to get the index into the session array.
        let session = desc
            .SessionNumber
            .checked_sub(1)
            .and_then(|n| sessions.get_mut(n as usize))
            .ok_or_else(|| anyhow!("Invalid session number"))?;

        match desc.Point {
            0xA0 => {
                // First Track Number/Disc Type
                session.disc_type = desc.Msf[1];
            }
            0xA1 => (), // Last Track Number (ignored)
            0xA2 => {
                // Start position of Lead-out
                session.leadout = Msf::from_bytes(desc.Msf).to_sector();
                if next_leadin < session.leadout {
                    next_leadin = session.leadout;
                }
            }
            0xB0 => {
                // Start time of next possible program
                next_leadin = Msf::from_bytes(desc.MsfExtra).to_sector();
            }
            0xC0 => (), // Start time of the first Lead-in Area of the disc (ignored)
            1..=99 => {
                // Track
                tracks.push(TrackInfo {
                    tno: desc.Point,
                    session: desc.SessionNumber,
                    control,
                    sector: Msf::from_bytes(desc.Msf).to_sector(),
                });
            }
            _ => log::warn!("TOC contains unhandled POINT value 0x{:X}", desc.Point),
        }
    }

    Ok((sessions, tracks))
}

fn read_ioctl_cdrom_raw_read(
    drive: HANDLE,
    lba: u32,
) -> Result<[u8; CD_RAW_SECTOR_WITH_SUBCODE_SIZE as usize]> {
    let read_cmd = RAW_READ_INFO {
        DiskOffset: lba as i64 * 2048,
        SectorCount: 1,
        TrackMode: RawWithSubCode,
    };

    let mut out_data = [0u8; CD_RAW_SECTOR_WITH_SUBCODE_SIZE as usize];
    let mut bytes_returned: u32 = 0;

    unsafe {
        DeviceIoControl(
            drive,
            IOCTL_CDROM_RAW_READ,
            Some(std::ptr::from_ref(&read_cmd).cast::<c_void>()),
            std::mem::size_of::<RAW_READ_INFO>() as u32,
            Some(out_data.as_mut_ptr().cast::<c_void>()),
            out_data.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }?;

    if bytes_returned != CD_RAW_SECTOR_WITH_SUBCODE_SIZE {
        log::warn!("Raw read only returned {} bytes", bytes_returned);
    }

    Ok(out_data)
}

impl WindowsDriveCdromBackend {
    pub fn new(path: &Path) -> Result<Self> {
        let handle = unsafe {
            CreateFileW(
                &HSTRING::from(path),
                GENERIC_READ.0,
                FILE_SHARE_READ,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }?;

        // Allow read commands to be sent directly to the driver without filtering.
        // Without this, Microsoft Virtual DVD-ROM will read all 0's on some sectors.
        let _ = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ALLOW_EXTENDED_DASD_IO,
                None,
                0,
                None,
                0,
                None,
                None,
            )
        };

        let capacity: usize = get_disk_length(handle)?.try_into()?;
        log::debug!("detected capacity: 0x{:X}", capacity);

        let (sessions, tracks) = read_full_toc(handle)
            .or_else(|e| {
                // Microsoft Virtual DVD-ROM supports Formatted TOC but not Full TOC.
                log::warn!(
                    "Failed to read Full TOC; falling back to Formatted TOC. Error: {}",
                    e
                );
                read_formatted_toc(handle)
            })
            .or_else(|e| {
                // Dang, we really can't read the TOC. Just treat the disc as one big data
                // track and hope for the best.
                log::warn!(
                    "Failed to read Formatted TOC; falling back to a data track. Error: {}",
                    e
                );
                let sector_count: u32 = capacity.div_ceil(2048).try_into()?;
                Ok::<(Vec<SessionInfo>, Vec<TrackInfo>), anyhow::Error>((
                    vec![SessionInfo {
                        number: 1,
                        disc_type: 0x00,
                        leadin: 0,
                        leadout: LBA_START_SECTOR + sector_count,
                    }],
                    vec![TrackInfo {
                        tno: 1,
                        session: 1,
                        control: DATA_TRACK,
                        sector: LBA_START_SECTOR,
                    }],
                ))
            })?;

        log::debug!("sessions: {:#?}", sessions);
        log::debug!("tracks: {:#?}", tracks);

        // XXX: Workaround an issue in Windows:
        // If ReadFile is performed on a drive handle before IOCTL_CDROM_RAW_READ,
        // IOCTL_CDROM_RAW_READ will stop working with "The parameter is incorrect."
        // This seems to fix it.
        let _ = read_ioctl_cdrom_raw_read(handle, 0);

        Ok(Self {
            path: path.into(),
            handle,
            capacity,
            sessions,
            tracks,
        })
    }
}

impl CdromBackend for WindowsDriveCdromBackend {
    fn byte_len(&self) -> usize {
        self.capacity
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
        // log::debug!("Reading 0x{:X} bytes from offset 0x{:X}", length, offset);

        // If move method is FILE_BEGIN, offset is treated as an unsigned value.
        unsafe { SetFilePointerEx(self.handle, offset as i64, None, FILE_BEGIN) }
            .map_err(|e| anyhow!(e))?;

        let mut result = vec![0; length];
        unsafe { ReadFile(self.handle, Some(result.as_mut_slice()), None, None) }
            .map_err(|e| anyhow!(e))?;

        Ok(result)
    }

    fn image_path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(&self.sessions)
    }

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&self.tracks)
    }

    fn read_raw_sector(&self, sector: u32) -> Result<RawSector> {
        // log::debug!("Reading raw sector {}", sector);
        let lba = sector
            .checked_sub(LBA_START_SECTOR)
            .ok_or_else(|| anyhow!("Tried to read from inaccessible sector {}", sector))?;
        let data = read_ioctl_cdrom_raw_read(self.handle, lba)?;
        Ok(RawSector {
            data: data[..RAW_SECTOR_LEN].try_into().unwrap(),
            control: DATA_TRACK, /* TODO */
        })
    }
}
