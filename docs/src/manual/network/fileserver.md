# Built-in file server

Snow can share a folder of your computer with the emulated Mac as an AppleShare
volume, with nothing to install or set up on the guest besides what the system
already has. The server appears as a second machine on a private LocalTalk network
that only the emulated Mac is connected to, so it is not reachable from anywhere else.

## Using it

1. In the **Network File Sharing…** menu, choose the shared folder with **Select
   folder...**, and name the volume if you want to.
2. Tick **Enable AppleShare file server**.
3. In the guest, make AppleTalk active and select the printer port in the Chooser or
   the Network control panel.
4. In the Chooser, click **AppleShare**, select the server, which has the volume's
   name (**Snow** unless you changed it), and click **OK**. Connect as a guest and
   pick the volume.

The server uses the printer port, so nothing else can use that port while it is enabled.
A serial bridge the port had comes back when the server is turned off, unless Snow was
closed or another workspace was loaded in the meantime.

Changing the folder, the volume name, **Read only**, the AFP version or an advanced
setting restarts the server while it is enabled. The guest loses its connection, and the
volume has to be mounted again from the Chooser. Wire debug logging is the exception: it
takes effect at once.

## Writing to the shared folder

The volume is **read-only** unless you untick **Read only** in the **Network File
Sharing…** menu. The change applies at once.

Mac files have more to them than a host file system stores: a resource fork, a type and
creator, and Finder information. A writable volume keeps these next to the files, in
`.AppleDouble` folders, the format [netatalk](https://netatalk.io) uses. Metadata
already there from netatalk, or in macOS `._` files, is read and updated where it is.
Read-only volumes write nothing at all.

A file copied from macOS can come with a companion file, named `._` followed by the
file's name. macOS keeps the file's type, creator and resource fork in it, and also its
extended attributes, such as Finder tags and the record of where the file was downloaded
from. Snow uses the companion file as it is, with one limit: after the emulated Mac saves
the file, locks or unlocks it, or changes its dates, macOS no longer finds the extended
attributes. The file's contents, type, creator and resource fork are never affected.

AppleSingle files can be read, but are read-only.

## File names

Mac names are at most 31 characters, in the MacRoman character set. A host file name
that does not fit shows as a shortened name holding a `#` and a hash, such as
`report#66cc8389.txt`. Names are case-insensitive on the Mac: if a folder holds both
`foo` and `Foo`, the guest sees both, but opening either by name may find the other.

Files and folders whose names begin with a dot are hidden; **Advanced…** can show them.

## Advanced settings

| Setting | |
|---|---|
| Maximum AFP version | The newest protocol version the server offers. 2.2 is the default, because some versions of Mac OS only ask for color icons over it. |
| Desktop scan depth, entry cap | Limits for the background scan that finds applications and their icons. |
| Hidden files | Whether dot-prefixed entries are hidden, shown as invisible, or shown. |
| Follow symlinks inside the share | Whether the scan follows them. Symbolic links leading out of the shared folder never open. |
| Wire debug logging | Logs every frame and command, to diagnose protocol problems. |

## Limitations

* The guest cannot log in as a registered user, only as a guest.
* There is no change notification: files changed on the host deep in a folder may not
  show up until the guest looks at that folder again.
* Comments set in **Get Info** and custom icons added through the Finder are not kept.
* File IDs last as long as the server runs, so aliases find a file again by name after
  the server restarts.
