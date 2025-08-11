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
Note that Snow will not automatically write back to your image file, even if an
image is not mounted write-protected.

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
 * <span class="material-symbols-rounded">save</span> indicates the floppy has not been written to.
 * <span class="material-symbols-rounded">save_as</span> indicates the floppy was written
to since it was mounted.

To save a floppy including any changes, use the 'Save image...' item. If the floppy
was ejected, you can use 'Save last ejected image...' to save the image from before
the floppy was ejected, including changes.

Note that Snow will never automatically write back to loaded floppy images - every
save must be done explicitly.

Floppy images are always saved in MOOF format.
