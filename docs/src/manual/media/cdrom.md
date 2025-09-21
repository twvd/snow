# CD-ROM drives

Snow supports CD-ROM drives in every emulated Macintosh model
except the Macintosh 128K/512K/512Ke (as these models never included SCSI).

Note that in order to use a CD-ROM drive, you need to have the 'Apple CD'
extension installed in the emulated operating system. This requires System 6
or higher, although System 7 is preferable (see 'Troubleshooting' below).

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
to browse for a CD image. Currently, ISO and TOAST files are supported.

It is also possible to drag a file of a supported image format into
the Snow emulator window, which will load it into the first available
empty CD-ROM drive.

## Creating an image out of files on the host system

Snow can create a temporary ISO image out of existing files on the
host system and mount it on an emulated CD-ROM drive as a means of
transfering files from the host to the emulated system.

To do this, use the 'Mount image from files...' menu item.

## Ejecting a CD

To eject a CD, use the eject function in the emulated operating system
(drag the CD to the trash can).

## Detaching a drive

To detach a CD-ROM drive, use the 'Detach' menu action.

Note that this is the equivalent of pulling the cable on a hard drive so
if the disk is in use by the emulated operating system, it will likely
crash. Shut down the emulated operating system first.

## Troubleshooting

On System 6, when you try to insert a CD, you may get the following message:
`Please unlock the disk "Disk name" and try again. The desktop file couldn't be created.`.
This is a System 6 issue which also occurs on real hardware. Install
the "Desktop Mgr" extension inside the emulated system to solve this, or
update to System 7.
