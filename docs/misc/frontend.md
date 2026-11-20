# Writing a frontend

The **core**, the `snow_core` crate, emulates a Mac. A **frontend** is the program around it: it
makes the machine, runs it, shows its screens, plays its sound and gives it the user's input.
`frontend_egui` is Snow's own frontend; other programs, such as Infinite Mac, use the core with
frontends of their own. This document describes the core's API as a frontend uses it, to make one
easier to write. It isn't exhaustive, and the API can change. How the devices of a machine work
inside the core is in the [device model](device-model.md).

## The words used here

A machine has **buses**. A bus has **slots**. A slot is empty, or holds a **device**. Each device
is of a **device model**, such as `DisplayCard824`, the Macintosh Display Card 8-24. Each device
model provides one **function**, such as Display or HardDisk: what the device does for the user.
The devices on the logic board, such as a built-in video, are on a bus too, called `Built-in`.

The [device model](device-model.md#terms) defines these terms in full. A **variant** is Rust's word
for one of the values of an enum: `DisplayCard824` is a variant of the enum `DeviceModel`.

A frontend does three things with them, in this order, and the next three sections follow it:

1. It asks the core what a Mac model has and what can go in it.
2. It describes the machine it wants: a `MachineConfig`, and one `DeviceConfig` per device.
3. It gives both to the core, which makes the machine.

## What a Mac model has

The core tells a frontend what a Mac model has and what each device model needs, so the frontend
needs little knowledge of its own about them. Nothing in this section needs a machine to exist.

### Its buses and slots

`MacModel::buses()` returns the buses of a Mac model:

```rust
pub struct Bus {
    pub kind: BusKind,          // Scsi, Swim, Nubus, LcPds, Se30Pds, IieDisk or BuiltIn
    pub id: String,             // Names this bus: "SCSI", "NuBus"
    pub title: String,          // For the menus
    pub slots: Vec<Slot>,
}

pub struct Slot {
    pub id: String,             // Names this slot on its bus: "3", "9", "video"
    pub device: Option<Device>, // The device this Mac has there without being asked, if any
}

pub struct Device {
    pub model: DeviceModel,       // Its device model
    pub rom: Option<PathBuf>,     // Its ROM file, if it has one
    pub function: FunctionConfig, // The Config of its function
    pub name: Option<String>,     // Its name for the menus, if the device gives one
}
```

`name` is what the menus call the device: "LaserWriter IISC", "Apple IIe UniDisk 3.5". The frontend
shows it as it is, and adds the device's status after it, such as the disk in a floppy drive. When
a device gives no name, the frontend writes the text itself, from the bus's `title`, the slot's
`id` and the device's function: "Floppy #1". The SCSI menu is a special case: the frontend always
writes the slot itself, and the device's name after it: "SCSI #5: LaserWriter IISC". A device only
gives a name once it exists: in what `MacModel::buses()` returns, every `name` is `None`.

For example, `MacModel::LC.buses()` gives:

| Bus id | Slot ids | Device there without being asked |
| --- | --- | --- |
| `SWIM` | `1`, `2` | `MacSuperDrive` in each |
| `SCSI` | `0` to `6` | None |
| `Built-in` | `video` | `V8Video` |
| `PDS_LC` | `0` | None |

So the frontend learns that an LC has a built-in video, one PDS slot, and seven places for SCSI
devices, without having any code about the LC.

### What can go in a slot

`models_for(bus.kind, mac_model)` lists the device models that fit a slot of that type of bus on
that Mac model. Each item is a `ModelInfo`: what the core knows about one device model. Every
device model has one, whether it is a card, a SCSI device or a device on the logic board, and
`DeviceModel::info()` returns it.

The fields of `ModelInfo` that a frontend reads:

| Field | What it says | What the frontend does with it |
| --- | --- | --- |
| `name` | The device model's name | Shows it in menus and dialogs |
| `rom` | The ROM file it needs, if any: a label, the digests of the known good files, and whether the device works without the file | Asks the user for the file, and checks it |
| `fixed` | It is part of the machine: never added or removed while the machine runs | Offers it when the machine is being described, not in the menus of a running machine |
| `function` | Its function, and what that function needs to know from the user | See below |

A slot that has a device without being asked, such as `Built-in` / `video`, isn't offered a choice
of device model. The frontend reads the `ModelInfo` of the device that is there
(`slot.device.model.info()`), to offer its settings.

## Functions: what to ask, the answers, and the status

The function is the only thing a frontend needs to look at to know what to show for a device. A
frontend has code for each function, and none for a device model: a new video card, with the
Display function, needs no frontend code.

Each function has up to three structs, defined next to each other in
`core/src/devices/function.rs`:

| Struct | What it is | Where the frontend finds it |
| --- | --- | --- |
| Info | **What the frontend needs to know** about a device model: mostly what to ask the user | `ModelInfo::function` |
| Config | **The answers.** What was chosen for one device | `DeviceConfig::function`, which the frontend fills in |
| Status | **What is so now.** What a device shows of itself while the machine runs | The `DeviceStatus` event |

**An Info conveys what the frontend needs to know about one device model, which is one specific
way of providing a function.** This covers two main cases:

- **What the frontend needs to fill in the dialog where the user first describes the machine**
  ("Load ROM..." in Snow's own frontend). For a display, this is what the device model supports:
  the monitors an 8-24 card can have on its connector, or the VRAM sizes of the built-in video of
  an LC. The user's choices then go in the Config.
- **What the frontend needs while the machine runs.** The Info of a floppy drive gives the
  characteristics of the type of drive used in a given machine: the types of image it supports,
  the sizes of blank disk it supports, and whether its speed can be adjusted.

An Info is static: it is fixed for a given device model, and never changes while the machine runs.
What changes is in the Status.

### The Display function: an Info for describing the machine

Display is the function of every video card and built-in video:

```rust
/// What to ask: what a device model with the Display function supports
pub struct DisplayInfo {
    /// What can be on its connector, `MacMonitor::NoMonitor` included, ordered by size. Empty:
    /// it has no connector, the screen is the machine's own.
    pub monitors: &'static [MacMonitor],
    /// The index in `monitors` of the monitor it has when none is chosen
    pub default_monitor: usize,
    /// The VRAM sizes it can have, in bytes, from the smallest. One: no choice.
    pub vram_sizes: &'static [usize],
    /// The index in `vram_sizes` of the size it has when none is chosen
    pub default_vram_size: usize,
}

/// The answers: what was chosen for one device
pub struct DisplayConfig {
    pub monitor: Option<MacMonitor>,   // One of DisplayInfo::monitors. None: the default monitor.
    pub vram_size: Option<usize>,      // One of DisplayInfo::vram_sizes. None: the default size.
}

/// What is so now
pub struct DisplayStatus {
    pub connected: bool,
}
```

So for a device whose function is Display, a frontend shows one menu of monitors and one of VRAM
sizes, filled from the `DisplayInfo`, and puts the user's choices in a `DisplayConfig`. It does the
same for the built-in video of an LC and for an 8-24 card in a Mac II.

**A video can have no monitor.** `MacMonitor` is what is on a video's connector, and one of its
variants is `MacMonitor::NoMonitor`: nothing connected. It is in the `DisplayInfo::monitors` of
every video that can detect that nothing is on its connector. A video that can't, such as the
Macintosh II Video Card, doesn't list it. With `NoMonitor`, the guest finds no monitor on that
video, and the device shows no display (`DisplayStatus::connected` is false). This is for a
machine with several videos, where the user wants a monitor on some of them only: on a card and
not on the built-in video, for example.

The Load ROM dialog offers "No monitor" for a video only when the Mac model has another slot that
can take a display. So it offers it on a Macintosh II and on a Quadra, and not on an LC, whose
built-in video is its only possible screen.

**A `monitor` of `None` is the video's default monitor**, which `DisplayInfo::default_monitor()`
returns: the monitor at the index `default_monitor` of its list. It doesn't mean "no monitor". So
a caller that chooses nothing gets a machine with a screen, and a video with no `DeviceConfig`
gets its default monitor too. For the colour videos of the Macs, it is the Macintosh 14"
high-res.

**A `vram_size` of `None` is the video's default size**, which `DisplayInfo::default_vram_size()`
returns. For today's videos it is the largest.

The lists are ordered by size, as the Load ROM dialog shows them; monitors of the same size are in
the order the device model gives them. The defaults are indices in them, so a default is always
one of the list. The core and the dialog both read them.

A video whose `DisplayInfo::monitors` is empty has no connector: its screen is the machine's own,
as on a Macintosh Plus. A frontend shows no menu of monitors for it, and its `monitor` isn't read.

**A machine needs at least one screen:** a video with a monitor other than `NoMonitor`, or a video
whose screen is the machine's own. The core checks it: making a machine in which every video has
`NoMonitor` fails. The Load ROM dialog shows an error in red in that case, and doesn't let the user
go on.

### The Floppy function: an Info used while the machine runs

A floppy drive has no Config: nothing is chosen for it when the machine is described, and a disk
is inserted while the machine runs. But the drives don't all accept the same disks or work the same
way, and the frontend's Devices menu has to follow that. The `FloppyInfo` of a drive's device model
says how:

```rust
/// What a floppy drive's device model accepts and can do
pub struct FloppyInfo {
    pub images: FloppyImages,                  // The image files it accepts
    pub blank_formats: &'static [FloppyType],  // The blank disks it can be given
    pub guest_ejects: bool,                    // The guest ejects its disks. False: the user does.
    pub rpm_control: bool,                     // Its speed can be changed
    pub writable: bool,                        // It writes to its image
}

pub enum FloppyImages {
    /// The Mac's floppy images
    Mac,
    /// Image files of another kind: what the file dialog calls them, and their extensions
    Files { label: &'static str, extensions: &'static [&'static str] },
}
```

| Field | What the Devices menu does with it | Where the drives differ |
| --- | --- | --- |
| `images` | Sets which files the file dialog offers, and the list of recent images: one per kind of file | The Mac's drives accept Mac floppy images. The 5.25" drive of an Apple IIe Card accepts `dsk`, `do`, `po`, `2mg` and `woz` files. |
| `blank_formats` | Greys the "Insert blank ..." items the drive can't accept | A SuperDrive accepts 400/800K and 1.44 MB blank disks, an 800K drive only the first. |
| `guest_ejects` | Shows "Force eject" when the guest ejects the disks, "Eject" when the user does | The Mac's drives eject on the guest's order. The Apple IIe Card's drives are ejected by hand. |
| `rpm_control` | Shows the speed adjustment | Only the 400K drive and the 800K (PWM) drive have it. |
| `writable` | Offers "Mount write-protected" as a choice, in the menu and in the file dialog; a drive that never writes always mounts so | The 5.25" drive of an Apple IIe Card never writes to its image. |

So the Devices menu has no code for a given drive: a new floppy drive with its own `FloppyInfo`
gets the right menu.

The menu tells two cases apart:

- **What a drive can't do right now is greyed**, such as "Eject" on an empty drive, or the
  1.44 MB blank disk on an 800K drive, which accepts other blank disks.
- **What a drive can never do isn't drawn.** A drive with no `blank_formats` has no "Insert
  blank ..." item. A drive whose image files aren't the Mac's (`FloppyImages::Files`) has none of
  the items that work on Mac floppy images: saving the disk, write-back and putting back the last
  ejected disk. So a drive of an Apple IIe Card shows "Load image...", "Load recent image" and
  "Eject".

Every floppy drive goes through the same file dialog code, given its kind of image: one dialog
per kind, kept so that it opens in the folder it was last in, with the "Mount write-protected"
checkbox (ticked and greyed for a drive that never writes), and one list of recent images per
kind. The Mac's dialog also shows what the selected image holds, and saves images.

### The three enums

The three are gathered in three enums, which have the same variants in the same order:

```rust
pub enum FunctionInfo {                  pub enum FunctionConfig {
    Floppy(FloppyInfo),                      Floppy,
    HardDisk,                                HardDisk(HardDiskConfig),
    Cdrom,                                   Cdrom(CdromConfig),
    Ethernet(EthernetInfo),                  Ethernet(EthernetConfig),
    Printer,                                 Printer(PrinterConfig),
    Display(DisplayInfo),                    Display(DisplayConfig),
    AppleIIeCard,                            AppleIIeCard(IieCardSettings),
}                                        }
```

`FunctionStatus` is the third. A variant with no struct has nothing to say on that side.

| Function | Info | Config: the answers | Status: what is so now |
| --- | --- | --- | --- |
| Floppy | `FloppyInfo`: which image files and blank disks the drive accepts, who ejects, whether its speed can change, whether it writes | Nothing: a disk is inserted while the machine runs | The disk's title, ejected or not, unsaved writes, write-back |
| Display | The monitors, the VRAM sizes | `monitor`, `vram_size` | Connected or not |
| HardDisk | Nothing | `media`: the image file, optional | The image file, the capacity |
| Cdrom | Nothing | `folder`: a folder of images the guest switches between, optional | The disc's image, the folder |
| Ethernet | `EthernetInfo`: the maker's prefix of its addresses | `mac`: the adapter's Ethernet address, or none for a random one that no other adapter of the machine has; `link`: what it is connected to on the host, `Down` for nothing | The address, the link, a capture in progress |
| Printer | Nothing | `output_dir`: the folder its pages go to, or none for the frontend's default folder | The folder in use |
| AppleIIeCard | Nothing | `IieCardSettings`, the card's own struct: its security jumper, and which drive models are on its disk port | Nothing |

An Ethernet adapter keeps its address: `mac` is the Ethernet address the guest reads from it. A
frontend that adds an adapter asks the core for a new address and writes it in the adapter's
`DeviceConfig`. The adapter then has the same address each time the machine is made, as a real
one does.

The Apple IIe Card is a function of its own because nothing describes what to ask for it yet: a
frontend that supports it has code written for that card.

The card makes its drives itself. Its `IieCardSettings` only says which drive models are on its
disk port, and the drives have no `DeviceConfig`. Once the machine is made, they are devices like
any other, on a bus that the card adds. Each has the `ModelInfo` of its device model, with its
`FloppyInfo`, so the Devices menu handles them as it does the Mac's own drives.

### The Config and Status structs of the other functions

The structs of the Display and Floppy functions are above. The Config structs of the other
functions:

```rust
pub struct HardDiskConfig {
    pub media: Option<PathBuf>,          // The image file. None: given later, by load_image().
}

pub struct CdromConfig {
    pub folder: Option<PathBuf>,         // A folder of disc images the guest switches between
}

pub struct EthernetConfig {
    pub mac: Option<[u8; 6]>,            // The adapter's Ethernet address. None: a random one
                                         // that no other adapter of the machine has.
    pub link: EthernetLinkType,          // What it is connected to on the host
}

pub struct EthernetInfo {
    pub oui: [u8; 3],                    // The maker's prefix of its addresses: Dayna's 00:80:19
}

pub enum EthernetLinkType {
    Down,                                // Nothing
    NAT,                                 // The host's network, through address translation
    NATHttpsStripping,                   // The same, with HTTPS turned into HTTP for the guest
    Bridge(u32),                         // A network interface of the host, by its index
    TapBridge(String),                   // A TAP interface of the host, by its name (Linux only)
}

pub struct PrinterConfig {
    pub output_dir: Option<PathBuf>,     // The folder its pages go to. None: the default folder.
}

pub struct IieCardSettings {
    pub security_jumper: bool,           // The security jumper is installed
    pub disk_chain: [IieDiskDevice; 2],  // The drive models on its disk port, in chain order
}

pub enum IieDiskDevice { None, Apple525, UniDisk35 }
```

The Status structs:

```rust
pub struct FloppyStatus {
    pub title: String,                   // The title of the disk in the drive
    pub ejected: bool,                   // No disk is in the drive
    pub dirty: bool,                     // The disk has writes that aren't saved to a file
    pub writeback_supported: bool,       // The disk can be written back to the file it came from
    pub writeback_enabled: bool,         // Its writes go to that file
}

pub struct HardDiskStatus {
    pub media: Option<PathBuf>,          // The image file. None: the image isn't a file.
    pub capacity: usize,                 // In bytes. 0: the disk has no image yet.
}

pub struct CdromStatus {
    pub image: Option<PathBuf>,          // The image of the disc in the drive. None: no disc.
    pub folder: Option<PathBuf>,         // Its folder of disc images, if it has one
}

pub struct EthernetStatus {
    pub mac: [u8; 6],                    // The adapter's Ethernet address
    pub link: EthernetLinkType,          // What it is connected to on the host
    pub capture: Option<EthernetCaptureStatus>,  // A capture of its packets to a file, if any
}

pub struct PrinterStatus {
    pub output_dir: PathBuf,             // The folder its pages go to
}
```

## Describing a machine

A machine is described by two things: one `MachineConfig`, and a list of `DeviceConfig`, one per
device.

### `MachineConfig`: the machine apart from its devices

```rust
pub struct MachineConfig<'a> {
    pub model: MacModel,
    pub rom: &'a [u8],
    pub extension_rom: Option<&'a [u8]>,
    pub ram_size: Option<usize>,
    pub mouse_mode: MouseMode,
    pub pmmu_enabled: bool,
    pub fpu_enabled: bool,
}
```

| Field | What it is |
| --- | --- |
| `model` | The Mac model |
| `rom` | The main ROM. The frontend reads the file and gives its contents. |
| `extension_rom` | A second ROM that some Mac models map next to the main one, optional. Also given as its contents. |
| `ram_size` | The RAM size in bytes. `None`: the Mac model's default. |
| `mouse_mode` | How the host's mouse drives the guest's: absolute, relative or disabled |
| `pmmu_enabled` | A 68851 in a Macintosh II |
| `fpu_enabled` | A 68882 in an LC family Mac |

`MachineConfig::new(rom, model)` fills in the defaults, so a frontend that sets fields with
`..MachineConfig::new(rom, model)` keeps building when fields are added.

### `DeviceConfig`: one device

```rust
pub struct DeviceConfig {
    pub bus: String,                // Which bus of the machine
    pub slot: String,               // Which slot of that bus
    pub model: Option<DeviceModel>, // Which device model goes there
    pub rom: Option<PathBuf>,       // The device's ROM file, if its device model needs one
    pub function: FunctionConfig,   // The answers for its function
}
```

The first three fields say which device, and where.

**`bus` is the id of one bus of the machine.** It is `Bus::id`, as `MacModel::buses()` gives it.
The ids are constants of the core, in `bus_id`. They are written in the files a frontend saves, so
they never change.

| Constant | Id | The bus |
| --- | --- | --- |
| `bus_id::SWIM` | `SWIM` | The floppy connectors |
| `bus_id::SCSI` | `SCSI` | The SCSI bus |
| `bus_id::NUBUS` | `NuBus` | The NuBus slots |
| `bus_id::PDS_LC` | `PDS_LC` | The processor-direct slot of the LC family |
| `bus_id::PDS_SE30` | `PDS_SE30` | The processor-direct slot of the SE/30 |
| `bus_id::IIE_DISK` | `Floppy:PDS_LC.0` | The drives of the Apple IIe Card that is in `PDS_LC` slot `0` |
| `bus_id::BUILT_IN` | `Built-in` | The devices on the logic board |

An id names one bus, not a type of bus: the type is `Bus::kind`. The drives of the Apple IIe Card
show the difference: their bus is of type `IieDisk`, and its id says which card it belongs to.
No `DeviceConfig` names that bus, as the card makes its drives itself. Its id is for the commands
and the status of the drives, once the machine is made.

**`slot` is the id of one slot on that bus.** It is `Slot::id`. Each bus has its own ids:

| Bus | Slot ids | What the id is |
| --- | --- | --- |
| `SWIM` | `1`, `2`, `3` | The drive's position, from 1 |
| `SCSI` | `0` to `6` | The SCSI ID |
| `NuBus` | `9` to `E` | The slot number, in hexadecimal |
| `PDS_LC`, `PDS_SE30` | `0` | The only slot |
| `Floppy:PDS_LC.0` | `1`, `2` | The drive's position on the card's chain, from 1 |
| `Built-in` | `video` | A name |

A slot id is a name. A frontend compares it, shows it and passes it back, and never computes with
it. A frontend that has a number of its own, such as a SCSI ID the user picked, makes the id from
it: the slot of SCSI ID 3 has the id `"3"`.

**`model` is the device model**, a variant of the enum `DeviceModel`:

```rust
pub enum DeviceModel {
    ScsiHardDisk, ScsiCdrom,
    #[cfg(feature = "ethernet")] DaynaPortScsiLink,
    #[cfg(feature = "printer")] LaserWriterIISC,
    Mac400KDrive, Mac800KDrive, Mac800KPwmDrive, MacSuperDrive,
    DisplayCard824, MacIIVideoCard, SE30Video, CompactVideo, PortableVideo,
    V8Video, SonoraVideo, DafbVideo, MemcJrVideo, DjMemcVideo,
    AppleIIePdsCard, UniDisk35, Apple525Drive,
}
```

It is optional: **`None` means the device the machine has in this slot.** This is how a
`DeviceConfig` for a built-in device is written: the machine already has the device, and the
`DeviceConfig` only carries what was chosen for it.

A saved file may name a device model that this build of the core doesn't have, for instance an
Ethernet adapter when the core was built without Ethernet. That one `DeviceConfig` can't be read:
it is reported as unusable, and the rest still loads.

The last two fields say how the device is set up.

**`rom` is the path of the device's ROM file.** It is needed when the `rom` field of the device
model's `ModelInfo` isn't `None`. The core reads the file.

**`function` holds the answers for the device's function**, as the
[section above](#functions-what-to-ask-the-answers-and-the-status) describes. Its variant is the
one of the device model's `FunctionInfo`.

Three examples:

```rust
// An 8-24 card in NuBus slot $9, with a 12" RGB monitor
DeviceConfig {
    bus: bus_id::NUBUS.into(),
    slot: "9".into(),
    model: Some(DeviceModel::DisplayCard824),
    rom: Some("8-24.rom".into()),
    function: FunctionConfig::Display(DisplayConfig {
        monitor: Some(MacMonitor::RGB12),
        vram_size: None,
    }),
}

// A hard disk at SCSI ID 0
DeviceConfig {
    bus: bus_id::SCSI.into(),
    slot: "0".into(),
    model: Some(DeviceModel::ScsiHardDisk),
    rom: None,
    function: FunctionConfig::HardDisk(HardDiskConfig {
        media: Some("system.img".into()),
    }),
}

// The built-in video of an LC, whichever device model that is, with a 12" RGB monitor and
// 512 KB of VRAM
DeviceConfig {
    bus: bus_id::BUILT_IN.into(),
    slot: built_in::VIDEO.into(),
    model: None,
    rom: None,
    function: FunctionConfig::Display(DisplayConfig {
        monitor: Some(MacMonitor::RGB12),
        vram_size: Some(512 * 1024),
    }),
}
```

A slot with no `DeviceConfig` gets what the Mac model has there without being asked: its usual
floppy drives, its built-in video with the default monitor, and nothing in a SCSI or card slot.

## Making a machine

```rust
use snow_core::devices::{DeviceConfig, DeviceModel, DisplayConfig, FunctionConfig, bus_id};
use snow_core::emulator::{Emulator, MachineConfig};
use snow_core::mac::{MacModel, MacMonitor};

let rom = std::fs::read("macii.rom")?;
let machine = MachineConfig {
    ram_size: Some(8 * 1024 * 1024),
    ..MachineConfig::new(&rom, MacModel::MacII)  // The defaults for the rest
};
let devices = vec![DeviceConfig {
    bus: bus_id::NUBUS.into(),
    slot: "9".into(),
    model: Some(DeviceModel::DisplayCard824),
    rom: Some("8-24.rom".into()),
    function: FunctionConfig::Display(DisplayConfig {
        monitor: Some(MacMonitor::RGB12),
        vram_size: None,
    }),
}];
let (mut emulator, displays, refused) = Emulator::new(&machine, devices)?;
```

What comes back:

- the emulator;
- one `DisplayHandle` per display, in order: a name, and the receiver its frames arrive at;
- the `DeviceConfig`s the machine couldn't use, each with the reason, for the frontend to show.

A Mac with no display, such as a Mac II with no video card, isn't made: `Emulator::new()` returns
an error.

## Running it

There are two ways.

**In a thread of its own**, as `frontend_egui` does. The frontend sends commands and reads events
through channels, and the thread only ticks the emulator:

```rust
let commands = emulator.create_cmd_sender();
let events = emulator.create_event_recv();
std::thread::spawn(move || loop {
    match emulator.tick(1, ()) {
        Ok(0) => break,      // The Quit command was received
        Ok(_) => (),
        Err(e) => panic!("Emulator error: {}", e),
    }
});
commands.send(EmulatorCommand::Run)?;
```

**In the frontend's own loop**, as Infinite Mac does. The frontend owns the emulator, calls
`tick()` itself, and can call the emulator's methods directly as well as send commands. After
calling a method that changes a device, it calls `status_update()`, which sends the events at once
instead of within the next 500 ms.

## Commands and events

Commands go to the emulator, events come back, each over its own channel. A small frontend needs
these commands:

| Command | What it does |
| --- | --- |
| `Run`, `Stop`, `Reset`, `Quit` | Runs, pauses, resets the machine, or ends the emulator's loop |
| `SetSpeed(speed)` | Real speed, or as fast as the host can |
| `KeyEvent(KeyEvent::KeyDown(code, map))` | A key of the Mac keyboard |
| `MouseUpdateRelative { relx, rely, btn }` | Mouse movement |
| `MouseUpdateAbsolute { x, y, display }` | A pointer position on a display, by its index in the list |

and these events:

| Event | What to do |
| --- | --- |
| `Buses(list)` | Keep the list of buses, and forget the statuses |
| `DeviceStatus(bus, slot, status)` | Keep the status of that slot |
| `Status(status)` | The machine's state: running or not, the speed |
| `UserMessage(kind, text)` | Show it to the user |

**An event the frontend doesn't know is to be ignored**: new events get added, and the core sends
them to every frontend.

## Devices while the machine runs

The frontend learns what the machine has from the `Buses` and `DeviceStatus` events, as the
[device model](device-model.md#how-the-frontend-learns-what-the-machine-has) explains. A status
always follows the list it belongs to. `emulator.buses()` gives the list too.

A slot is named in commands and events by the id of its bus and the id of the slot, the same two
ids as in a `DeviceConfig` ([`DeviceConfig`: one device](#deviceconfig-one-device)).

```rust
// An image in the floppy drive in slot 1 of the SWIM bus
commands.send(EmulatorCommand::LoadMedia(
    bus_id::SWIM.into(),
    "1".into(),
    "system.img".into(),
    false,
))?;
```

A device is added with a `DeviceConfig`, the same struct as when the machine is made, and removed
by the ids of its bus and slot. Both can be done by command or by method:

```rust
let disk = DeviceConfig {
    bus: bus_id::SCSI.into(),
    slot: "0".into(),
    model: Some(DeviceModel::ScsiHardDisk),
    rom: None,
    function: FunctionConfig::HardDisk(HardDiskConfig { media: Some("hd.img".into()) }),
};
emulator.add_device(&disk)?;                 // Or commands.send(EmulatorCommand::AddDevice(disk))
emulator.remove_device(bus_id::SCSI, "3")?;  // Or EmulatorCommand::RemoveDevice(..)
```

A device is never replaced: `add_device()` takes an empty slot only, and a `DeviceConfig` for a
slot that holds a device is an error. The helpers by SCSI ID below work the same way. To put
another device there, remove the one in the slot first with `remove_device()`. What can change on
a device that is there has a command of its own, as every method of a function has:
`EthernetSetLink` for an Ethernet link, `CdromSetFolder` for a CD-ROM drive's folder.

The media commands are `LoadMedia`, `EjectMedia`, `InsertFloppyImage` (a floppy image already in
memory) and `BranchDisk` (a hard disk carries on from a copy of its image). `LoadMedia` on a hard
disk replaces its image, on a drive that offers it (`HardDiskFunction::load()`, such as an HD20 on
its chain); a SCSI hard disk refuses it, and is removed and added instead.

**The Devices menu** of Snow's frontend draws the list of buses in three parts:

1. the floppy drives, by bus: the Mac's, then those of the Apple IIe Card;
2. the SCSI bus, by ID, each ID with its device, whatever its function;
3. the devices on the other buses, one section per function, sorted by bus within a function: for
   example the HD20s on a Mac's floppy port under the hard disks, and a built-in Ethernet and an
   Ethernet card under Ethernet. A section is shown only when it has something.

The items of a function are the same wherever the device is. A hard disk whose device model is
`fixed` offers "Load image..." when it has no image and "Change image..." when it has one; one that
isn't `fixed` offers to remove it.

`emulator.devices()` reports the machine as a list of `DeviceConfig` it can be made from again,
which is what a frontend saves. Each `DeviceConfig` is as the frontend gave it, with the changes
the user asked for since: a hard disk that was branched has its new image file, and an Ethernet
adapter its new link. What the core worked out by itself isn't in it: a printer that was given no
`output_dir` still has none, whichever folder its pages go to. The one exception is an Ethernet
adapter made with no address: it reports the random address it got, so that it keeps it the next
time.

The list comes from the list of buses, where each slot's `Device` has the device's Config as it is
now. The core sends the `Buses` event again when a device is added or removed, and when a command
changes a Config. A frontend that runs the emulator in a thread makes the same list from the last
`Buses` event it received.

## Images the frontend keeps itself

A frontend whose disk images aren't files, such as Infinite Mac's, which keeps them in the
browser, implements the `DiskImage` trait and hands the object to the core:

```rust
emulator.attach_disk_image_at(Box::new(my_disk), 0)?;     // A hard disk at SCSI ID 0
emulator.attach_cdrom(3)?;                                 // An empty CD-ROM drive at ID 3
emulator.insert_cdrom_image_at(Box::new(my_disc), 3)?;     // A disc in it
emulator.status_update()?;
```

`attach_disk_image_at()` adds a hard disk whose Config names no file, then gives it the image with
`HardDiskFunction::load_image()`. The guest never sees the disk without its image. A disk attached
this way is a device like any other, in `buses()` and in save states. Its `DeviceConfig`, from
`devices()`, has no `media`: such a disk can't come back from a workspace.
`attach_printer(scsi_id, folder)` puts a LaserWriter IISC at a SCSI ID, which writes its pages to
the folder.

## Frames, sound and input

- **Frames:** each `DisplayHandle` has a `receiver`, where the latest frame of that display is left
  for the frontend to take. A machine has one display, or more with video cards
  ([displays.md](displays.md)).
- **Sound:** the frontend implements `AudioProvider`, which makes an `AudioSink` the machine sends
  its samples to, and gives it with `emulator.set_audio_provider(Arc::new(Mutex::new(provider)))`.
- **Mouse:** relative or absolute, by command. An absolute position names its display.
- **Keyboard:** `KeyEvent` commands, with the Mac key code.

## What belongs to the host

Some things belong to the host, and the machine only uses them: a folder, a file, the sound
output, the date. **None of them is an argument of `Emulator::new()`.** The frontend gives each one
by a method of the emulator or by a command, once the emulator exists and before it starts running
the machine. It does so in the same way for a machine made by `Emulator::new()` and for one
restored from a save state.

| What | How the frontend gives it |
| --- | --- |
| The PRAM file | `emulator.persist_pram(path)` |
| The date and time | `emulator.set_datetime(datetime)` |
| The sound output | `emulator.set_audio_provider(provider)` ([Frames, sound and input](#frames-sound-and-input)) |
| The shared folder of the BlueSCSI Toolbox | `EmulatorCommand::SetSharedDir` |
| The default folder for printed pages | `EmulatorCommand::SetDefaultPrintDir` |
| What each serial port is connected to | `EmulatorCommand::SerialBridgeEnable`, `SerialBridgeDisable` |

**The default folder for printed pages** is for a printer whose `PrinterConfig` has no
`output_dir`. Such a printer writes its pages to the folder the frontend gave last, and its Status
says which folder that is. Snow's own frontend gives the host's Desktop folder. Until a frontend
gives one, the folder is the current directory of the process.

**What a serial port is connected to isn't a device**, and no `DeviceConfig` describes it. It is a
serial bridge, which the frontend enables for a port with `SerialBridgeEnable(port, config)`. The
`SerialBridgeConfig` says what the port is connected to on the host:

| `SerialBridgeConfig` | The port is connected to |
| --- | --- |
| `Pty` | A pseudo-terminal on the host (Unix only) |
| `Tcp(port)` | A TCP port the core listens on |
| `LocalTalk` | LocalTalk over UDP multicast, which other emulators on the host's network join |
| `AppleTalk { .. }` | A private LocalTalk network with Snow's AppleShare file server on it, with the folder it shares and its settings |

The core reports the state of a port's bridge with the `SerialBridgeStatus` event. A machine has
no bridge when it is made, nor when it is restored from a save state. Snow's own frontend enables
the bridges of its workspace after it makes a machine.

The frontend also removes the temporary files a restored save state made, when it is done with the
machine: `emulator.temp_files()` lists them.

## Cargo features

The core builds without its optional features. They are:

| Feature | Adds |
| --- | --- |
| `savestates` | Saving and restoring the machine |
| `mmap` | Disk images mapped in memory; without it, disks are read-only |
| `ethernet` (and `ethernet_nat`, `ethernet_tap`, `ethernet_raw`) | The DaynaPORT and its links |
| `printer` | The LaserWriter IISC |
| `afp_server` | The AppleTalk file server behind a serial bridge |

Infinite Mac builds the core with `printer` only.
