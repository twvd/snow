//! SCSI LaserWriter IISC printer emulation
//!
//! Captures print jobs as PNG files

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::debuggable::Debuggable;
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetCommon, ScsiTargetEvent, ScsiTargetType};
use crate::mac::scsi::{STATUS_CHECK_CONDITION, STATUS_GOOD, ScsiCmdResult};

#[cfg(feature = "printer")]
use chrono::Local;
#[cfg(feature = "printer")]
use image::{GrayImage, Luma};

/// LaserWriter IISC SCSI printer emulator
#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetPrinter {
    /// Pending 0x05 header awaiting bitmap data
    mask_header: Option<[u8; 10]>,

    /// Current page image being drawn into
    #[cfg(feature = "printer")]
    #[serde(skip)]
    framebuffer: Option<GrayImage>,

    /// Page dimensions - width in pixels
    page_width: u16,

    /// Page dimensions - height in pixels
    page_height: u16,

    /// Viewport offset X (for regional drawing, currently unused)
    viewport_x: u16,

    /// Viewport offset Y (for regional drawing, currently unused)
    viewport_y: u16,

    /// Output directory for PNG files
    output_dir: PathBuf,

    /// Page counter for debug or stats
    page_count: u16,

    /// Number of render_mask() calls on the current page
    mask_count: usize,

    /// Event stuff for the "page saved" notification
    #[serde(skip)]
    pending_event: Option<ScsiTargetEvent>,

    /// SNOW_SCSI_TRACE_PRINTER env flag, similar stuff in controller.rs
    #[serde(skip)]
    trace_flag: bool,

    common: ScsiTargetCommon,
}

impl Default for ScsiTargetPrinter {
    fn default() -> Self {
        Self {
            mask_header: None,
            #[cfg(feature = "printer")]
            framebuffer: None, // need image crate
            page_width: 2550,  // ~300 DPI letter width (8.5")
            page_height: 3300, // ~300 DPI letter height (11")
            viewport_x: 0,
            viewport_y: 0,
            output_dir: PathBuf::from("/tmp/"),
            page_count: 0,
            mask_count: 0,
            pending_event: None,
            trace_flag: false,
            common: Default::default(),
        }
    }
}

impl ScsiTargetPrinter {
    pub(super) fn new(output_dir: PathBuf) -> Self {
        let env_flag = |name: &str| {
            std::env::var(name)
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(false)
        };
        Self {
            output_dir,
            trace_flag: env_flag("SNOW_SCSI_TRACE_PRINTER"),
            ..Default::default()
        }
    }

    // private hexdump-like routine. Only used when SNOW_SCSI_TRACE_PRINTER is set
    fn trace_dump(data: &[u8]) -> String {
        if data.len() <= 16 {
            data.iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            format!(
                "{} .. ({} bytes)",
                data.iter()
                    .take(16)
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" "),
                data.len()
            )
        }
    }

    fn paper_name(&self) -> &'static str {
        match (self.page_width, self.page_height) {
            (2400, 3175) => "US Letter (8.0\"x10.6\")",
            (2000, 3750) => "US Legal (6.72\"x12.5\")",
            (2400, 3375) => "A4 (8.0\"x11.27\")",
            (2000, 2825) => "B5 (6.67\"x9.43\")",
            (1136, 2725) => "#10 Envelope (3.84\" x 9.1\")",
            _ => "Custom",
        }
    }

    /// Draw monochrome bitmap mask data into framebuffer
    #[cfg(feature = "printer")]
    fn render_mask(&mut self, header: &[u8; 10], bitmap: &[u8]) {
        self.mask_count += 1;
        let x1 = u16::from_be_bytes([header[0], header[1]]);
        let y1 = u16::from_be_bytes([header[2], header[3]]);
        let x2 = u16::from_be_bytes([header[4], header[5]]);
        let y2 = u16::from_be_bytes([header[6], header[7]]);
        let cmd_type = header[9];

        let width = x2.saturating_sub(x1);
        let height = y2.saturating_sub(y1);
        let bytes_per_row = (width + 7) / 8;

        if self.trace_flag {
            // B = black mask, W = white mask, U = unknown mask
            let type_ch = match cmd_type {
                0x01 => 'B',
                0x03 => 'W',
                _ => 'U',
            };
            log::debug!(
                "render_mask: ({}, {}) ⇾ ({}, {}) {} {}x{} {} bytes",
                x1,
                y1,
                x2,
                y2,
                type_ch,
                width,
                height,
                bitmap.len()
            );
        }

        if self.framebuffer.is_none() {
            self.framebuffer = Some(GrayImage::from_pixel(
                self.page_width as u32,
                self.page_height as u32,
                Luma([255u8]),
            ));
        }
        let image = self.framebuffer.as_mut().unwrap();

        // Render monochrome bitmap mask data
        for row in 0..height {
            for col in 0..width {
                let byte_idx = row as usize * bytes_per_row as usize + col as usize / 8;
                if byte_idx >= bitmap.len() {
                    break;
                }
                let bit = (bitmap[byte_idx] >> (7 - (col % 8))) & 1;
                let px = (x1 + col).min(self.page_width.saturating_sub(1)) as u32;
                let py = (y1 + row).min(self.page_height.saturating_sub(1)) as u32;
                if bit == 0 {
                    continue;
                }
                let color = if cmd_type & 0x02 != 0 {
                    Luma([255u8]) // white
                } else {
                    Luma([0u8]) // black
                };
                image.put_pixel(px, py, color);
            }
        }
    }

    /// Write current page image buffer to a PNG file
    #[cfg(feature = "printer")]
    fn render_to_png(&mut self) -> Result<()> {
        let ts = Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
        let filename = format!("Snow print {}.png", ts);
        let filepath = self.output_dir.join(&filename);

        log::info!(
            "LaserWriter: saving page {} to {}",
            self.page_count,
            filepath.display()
        );

        let image = self.framebuffer.get_or_insert_with(|| {
            GrayImage::from_pixel(
                self.page_width as u32,
                self.page_height as u32,
                Luma([255u8]),
            )
        });
        image.save(&filepath)?;

        log::info!("page saved: {}", filename);
        self.pending_event = Some(ScsiTargetEvent::PageSaved(filename));
        Ok(())
    }

    #[cfg(not(feature = "printer"))]
    fn render_to_png(&mut self) -> Result<()> {
        log::warn!("PNG rendering not available (printer feature disabled)");
        Ok(())
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetPrinter {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, _imgfn: &Path) -> Result<()> {
        todo!()
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        self.pending_event.take()
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Printer
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        if self.trace_flag {
            log::debug!("LaserWriter: TEST UNIT READY ⇾ OK");
        }
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    // Never called in practice: the Mac driver sends the CDB with PF=0 (cmd[1]=0x00),
    // which causes target.rs to return STATUS_GOOD before reaching this.
    fn mode_select(&mut self, page: u8, data: &[u8]) -> Result<()> {
        if self.trace_flag {
            log::debug!(
                "LaserWriter: MODE page={:02X} [{}]",
                page,
                Self::trace_dump(data)
            );
        }
        Ok(())
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
        // Complete response includes vendor-specific bytes from ROM @ 0x080C0E-0x080C17
        // This is checked against the same stuff within the "PDEF 125" resource
        let mut result = vec![0; 44];

        result[0] = 0x02; // Peripheral device type: Printer
        result[1] = 0x00; // Device type qualifier: Non-removable
        result[2] = 0x00; // ANSI version: 0x00 = Pre-SCSI-2
        result[3] = 0x00; // Response data format: 0x00 = SCSI-1 CCS
        // Additional length: 0x1F = 31 bytes follow (standard SCSI-1)
        // (vendor specific bytes are "extra" beyond that)
        result[4] = 0x1F;

        // Vendor identification (8 bytes): "APPLE   "
        result[8..16].copy_from_slice(b"APPLE   ");

        // Product identification (16 bytes): "PERSONAL LASER  "
        result[16..32].copy_from_slice(b"PERSONAL LASER  ");

        // Product revision (4 bytes): "1.00". Doesn't seem to matter to the driver
        // ROM Revision usage unknown, but better preserve it just in case.
        // Personal LaserWriter SC probably has a different revision
        result[32..36].copy_from_slice(b"1.00");

        // Vendor-specific bytes (8 bytes): From ROM @ 0x080C0E-0x080C17
        // These appear in TattleTech as "ROM Revision: $000000FE202720FF"
        result[36..44].copy_from_slice(&[0x00, 0x00, 0x00, 0xFE, 0x20, 0x27, 0x20, 0xFF]);

        // Honor the initiator's allocation length
        let alloc = cmd.get(4).copied().unwrap_or(0) as usize;
        if alloc > 0 && alloc < result.len() {
            result.truncate(alloc);
        }

        if self.trace_flag {
            log::debug!(
                "LaserWriter: INQUIRY [{}] ⇾ DataIn [{}]",
                Self::trace_dump(cmd),
                Self::trace_dump(&result)
            );
        }
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, _page: u8) -> Option<Vec<u8>> {
        None
    }

    fn ms_density(&self) -> u8 {
        0
    }

    fn ms_media_type(&self) -> u8 {
        0
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn blocksize(&self) -> Option<usize> {
        None
    }

    fn blocks(&self) -> Option<usize> {
        None
    }

    fn read(&mut self, _block_offset: usize, _block_count: usize) -> Result<ScsiCmdResult> {
        unreachable!()
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        unreachable!()
    }

    fn image_fn(&self) -> Option<&Path> {
        None
    }

    fn load_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    fn load_image(
        &mut self,
        _image: Box<dyn crate::mac::scsi::disk_image::DiskImage>,
    ) -> Result<()> {
        unreachable!()
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    /// Handle LaserWriter IISC SCSI commands for now, maybe others later
    fn specific_cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            0x04 => {
                /*
                 * FORMAT printer-specific page format command
                 * cmd[1] observed as 0x00 or 0x02; cmd[4] = transfer length (4 bytes)
                 * not used in Snow for now, but maybe show it in a debug area later (TODO)
                 */
                if let Some(data) = outdata {
                    log::debug!("LaserWriter: FORMAT data [{}]", Self::trace_dump(data));
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::DataOut(cmd[4] as usize))
                }
            }
            0x05 => {
                /*
                 * READ BLOCK LIMITS
                 * Two-stage bitmap transfer:
                 * Phase 1 (outdata=None): request 10-byte header
                 * Phase 2 (outdata=header, mask_header=None): save header, request bitmap bytes
                 * Phase 3 (outdata=bitmap, mask_header=Some): render and return status
                 */
                match outdata {
                    None => Ok(ScsiCmdResult::DataOut(10)),
                    Some(data) if self.mask_header.is_none() => {
                        if data.len() != 10 {
                            log::warn!("LaserWriter: 0x05 header wrong size {}", data.len());
                            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                        }
                        let header: [u8; 10] = data.try_into().unwrap();
                        let width = u16::from_be_bytes([header[4], header[5]])
                            .saturating_sub(u16::from_be_bytes([header[0], header[1]]))
                            as usize;
                        let height = u16::from_be_bytes([header[6], header[7]])
                            .saturating_sub(u16::from_be_bytes([header[2], header[3]]))
                            as usize;
                        let bitmap_len = ((width + 7) / 8) * height;
                        self.mask_header = Some(header);
                        Ok(ScsiCmdResult::DataOut(bitmap_len))
                    }
                    Some(bitmap) => {
                        let header = self.mask_header.take().unwrap();
                        #[cfg(feature = "printer")]
                        self.render_mask(&header, bitmap);
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                }
            }
            0x06 => {
                // SETUP: receives region dimensions
                if let Some(data) = outdata {
                    let y_offset = u16::from_be_bytes([data[0], data[1]]);
                    let x_offset = u16::from_be_bytes([data[2], data[3]]);
                    let width = u16::from_be_bytes([data[4], data[5]]);
                    let height = u16::from_be_bytes([data[6], data[7]]);

                    if x_offset == 0 && y_offset == 0 {
                        // Full page setup: update page dimensions
                        self.page_width = width;
                        self.page_height = height;
                        self.viewport_x = 0;
                        self.viewport_y = 0;

                        // Identify commons paper size. Sizes from Service Manual
                        let paper_name = self.paper_name();

                        log::info!(
                            "LaserWriter: Page setup {}x{} pixels: {}",
                            width,
                            height,
                            paper_name
                        );

                        // Fresh image buffer for the new page
                        #[cfg(feature = "printer")]
                        {
                            self.framebuffer = Some(GrayImage::from_pixel(
                                self.page_width as u32,
                                self.page_height as u32,
                                Luma([255u8]),
                            ));
                        }
                    } else {
                        // Viewport setup: set drawing region offset ? Unused for now
                        self.viewport_x = x_offset;
                        self.viewport_y = y_offset;

                        if self.trace_flag {
                            log::debug!(
                                "LaserWriter: Viewport {}x{} at offset {}, {}",
                                width,
                                height,
                                x_offset,
                                y_offset
                            );
                        }
                    }

                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::DataOut(cmd[4] as usize))
                }
            }
            0x08 => {
                // READ(6): not valid for a printer; ROM issues a 1-block probe at boot
                if self.trace_flag && cmd[4] > 1 {
                    log::warn!("LaserWriter: invalid READ(6) of size {}", cmd[4]);
                }
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
            0x0A => {
                /*
                 * PRINT: bit 7 of cmd[5] signals print
                 * System 6 sends 0xA0, System 7 sends 0x80
                 * When printing multiple copies of the same page, 0xC0 might sneak in
                 */
                if (cmd[5] & 0x80) != 0 {
                    log::info!("LaserWriter: printing page {}", self.page_count + 1);
                    self.page_count += 1;
                    if let Err(e) = self.render_to_png() {
                        log::error!("LaserWriter: render failed: {}", e);
                    }
                    #[cfg(feature = "printer")]
                    {
                        self.framebuffer = None;
                    }
                }

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }

            _ => {
                // "Silently" discard all unknown stuff (SCSI tools probes, etc)
                log::warn!("LaserWriter: unknown command 0x{:02X}", cmd[0]);
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
        }
    }

    #[cfg(feature = "ethernet")]
    fn eth_set_link(&mut self, _link: super::ethernet::EthernetLinkType) -> Result<()> {
        unreachable!()
    }

    #[cfg(feature = "ethernet")]
    fn eth_link(&self) -> Option<super::ethernet::EthernetLinkType> {
        None
    }
}

impl Debuggable for ScsiTargetPrinter {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_str, dbgprop_udec};

        vec![
            dbgprop_udec!("Mask operations", self.mask_count),
            dbgprop_str!("Paper size", self.paper_name()),
            dbgprop_udec!("Pages printed", self.page_count),
        ]
    }
}
