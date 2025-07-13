# CD-ROM drives

Snow supports CD-ROM drives in every emulated Macintosh model
except the Macintosh 128K/512K (as these models never included SCSI).

Note that in order to use a CD-ROM drive, you need to have the 'Apple CD'
extension installed in the emulated operating system.

<div class="warning">
Most operating systems seem to only support a single CD-ROM drive. Snow
allows you to add more, but you may experience issues or non-functional
drives.
</div>

## Attaching a CD-ROM drive

To attach a drive, use the 'Drives > SCSI #n > Attach CD-ROM drive'
(where 'n' is the SCSI ID of the drive (0 to 6)) on an
unused SCSI slot. You can either load an image immediately or attach an
empty drive.

Note that you need to restart the emulated system for the drive to be
recognized.

## Mounting an existing image

To mount an existing image file, use the 'Load image...' menu action
to browse for a CD image. Note that an image file must be a multiple of
2048 bytes. Currently, only ISO files are supported.

## Ejecting a CD

To eject a CD, use the eject function in the emulated operating system
(drag the CD to the trash can).

## Detaching a drive

To detach a CD-ROM drive, use the 'Detach' menu action.

Note that this is the equivalent of pulling the cable on a hard drive so
if the disk is in use by the emulated operating system, it will likely
crash. Shut down the emulated operating system first.
