# Floppies

Snow supports 2 or 3 floppy drives, depending on the model you are emulating.
The following drives are available, per model:

 * Macintosh 128K/512K: 2 400K drives
 * Macintosh 512Ke: 2 800K drives
 * Macintosh Plus: 2 800K drives
 * Macintosh SE (non-FDHD): 3 800K drives
 * Macintosh SE (FDHD): 3 1.44MB SuperDrives
 * Macintosh Classic: 2 1.44MB SuperDrives
 * Macintosh II (non-FDHD): 2 800K drives
 * Macintosh II (FDHD): 2 1.44MB SuperDrives

PC floppies can be used in the models with SuperDrives, provided you have the
'PC Exchange' extension installed within the emulated system.

## Supported image types

Snow supports sector, bitstream and flux image files. Raw flux (e.g. A2R) is
also supported, but since these images are not ment for emulator use, it is
recommended to convert them to resolved flux or bitstream (if flux accuracy
is not required) first.

Writing to flux images is (currently) not supported, these will always be
mounted as write-protected.

Snow supports the following image file formats for reading:
* Apple DiskCopy 4.2 (sector-based)
* Apple Disk Archive / Retrieval Tool ('DART') (sector-based)
* Applesauce A2R 2.x and 3.x (raw flux)
* Applesauce MOOF (bitstream and flux)
* PCE Flux Image (PFI, flux)
* PCE Raw Image (PRI, bitstream)
* Raw images (sector-based)
* Any format (Mac 1.44MB or PC) supported by [Fluxfox](https://github.com/dbalsom/fluxfox)

Snow supports saving images to Applesauce MOOF format.

## Loading a floppy image

To load a floppy image, go to the 'Drives' menu, to the drive you want to use,
and select 'Load image' to browse to the image you want to load.

In the image browser you can view the metadata for the selected image on the right
side. To mount an image as write-protected, check the 'Mount write-protected' box.
For writable mounts, see [Automatic writeback](#automatic-writeback) below for
how and when changes are written to the disk image.

You can also mount an empty (400K/800K only currently) floppy which you can
format and write to by selecting 'Insert blank 400K/800K floppy'.

## Ejecting floppies

Floppies should preferably be ejected gracefully through the emulated operating
system. To do this, drag the floppy icon on the desktop to the trash.

If your emulated system ejected your floppy and you want to re-insert it
including any changes that were made, you can click 'Re-insert last ejected floppy'
in the menu for that drive.

Note: if you do not use 'Re-insert last ejected floppy' but instead load the
original image after the OS has written to it, the OS will not recognize it as
the same floppy!

To forcibly eject a floppy, use the 'Force eject' menu item. This is equivalent
to using a paperclip to forcibly eject a floppy on real hardware.

## Writing to floppies

When a floppy is written to, the icon in the menu bar will change:
 * <span class="material-symbols-rounded">save</span> indicates the floppy has
   not been written to.
 * <span class="material-symbols-rounded">save_as</span> indicates the floppy
   was written to since it was mounted.
 * <span class="material-symbols-rounded">autorenew</span> indicates
   [automatic writeback](#automatic-writeback) is enabled for the floppy.

To save a floppy including any changes, use the 'Save image...' item. If the floppy
was ejected, you can use 'Save last ejected image...' to save the image from before
the floppy was ejected, including changes.

Floppy images are always saved in MOOF format.

## Automatic writeback

Snow can also write changes back to a floppy image's source file automatically,
as the emulated system writes to it. With writeback enabled for a drive, you do
not need to remember 'Save image...' - the source file is automatically saved.

Writeback is only supported for MOOF images, since MOOF is the only format
Snow can write. For other formats, Snow can offer to convert the image to a
sibling MOOF on load (see [Behavior on floppy load](#behavior-on-floppy-load)
below).

### Enabling writeback for a drive

When writeback is on for a drive, the floppy icon in the menubar shows
<span class="material-symbols-rounded">autorenew</span>. You can toggle writeback
for the currently loaded image at any time via the 'Auto-save changes (writeback)'
checkbox in the drive's submenu under 'Drives'. The option is disabled when
the loaded image is not a MOOF.

### Behavior on floppy load

When loading a floppy, Snow may prompt you before the image is inserted:

 * **A sibling MOOF exists.** If you load a non-MOOF image and a `.moof` file
   with the same filename exists alongside it, Snow asks whether to load the MOOF
   file instead.
 * **The image can be converted to MOOF.** If the image format does not support
   writeback but the image can be converted to MOOF (sector or bitstream
   images, but not flux-only images), Snow offers to write a sibling `.moof`
   and load that. The original file is left untouched.
 * **Enable writeback?** Once the loaded image supports writeback, Snow asks
   whether to enable writeback for this image.

The default behavior for each prompt is configurable under 'Options > Floppy':

 * **Writeback** (Ask each time / Always enable / Never enable) - what Snow
   does when a writeback-capable image is loaded.
 * **Convert to MOOF** (Ask each time / Always convert / Never convert) -
   whether to convert non-MOOF images to a sibling MOOF on load.

Each prompt has a 'Remember my choice' option that updates the corresponding
setting.

### Backups

Writeback overwrites the source file. To make it easy to revert, enable
'Options > Floppy > Back up floppy image before enabling writeback'. With this
on, Snow copies the image to a timestamped sibling in the same directory just
before writeback is enabled for it, e.g. `MyDisk-bak 2026-05-14 130700.moof`.
The backup is taken once per writeback activation, not on every write.
