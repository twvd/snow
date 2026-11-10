# The device model

Everything a user can configure on a machine is a device in a slot on a bus, and each device
provides one function. This document describes that model: the structures in the core, how the
frontend uses them, how a machine is created from a list of devices, and how workspaces and save
states keep them.

An example machine, with some devices that don't exist yet to show where they would go:

| Bus | Slot | Device (a model) | Function |
|---|---|---|---|
| SCSI | ID 0 | ScsiHardDisk | HardDisk |
| SCSI | ID 3 | ScsiCdrom | Cdrom |
| SCSI | ID 4 | DaynaPortScsiLink | Ethernet |
| SWIM | 1 internal | Mac800KDrive | Floppy |
| SWIM | 2 external | Mac800KDrive | Floppy |
| SWIM | HD20.1 | Hd20 | HardDisk |
| SWIM | HD20.2 | *Empty, can take an Hd20* | |
| NuBus | Slot 9 | DisplayCard824 | Video |
| NuBus | Slot A | A SCSI card (later) | **Card, adds a bus** (the card's bus joins the same flat list) |
| NuBus | Slot B | A NuBus Ethernet card (later) | Ethernet |
| SCSI:NuBus.A | ID 0 | ScsiHardDisk | HardDisk |
| PDS_LC | 0 | AppleIIePdsCard | **AppleIIe, adds a bus** (the card's drives are a bus in the same flat list) |
| Floppy:PDS_LC.0 | 1 | UniDisk35 | Floppy |
| Floppy:PDS_LC.0 | 2 | Apple525Drive | Floppy |

Built-in controllers that only provide a bus and have no settings, such as the 5380, aren't in
the list. A built-in device with settings, such as the SE/30's video with its ROM, is in a slot of
the `Built-in` bus.

Several device models can provide the same function: `ScsiHardDisk` and `Hd20` both provide
HardDisk, and a NuBus Ethernet card would provide Ethernet like `DaynaPortScsiLink`. A card that
adds a bus only adds a row to the same flat list.

**The core produces the bus list.** It describes the running machine as a list of buses, with
their slots and devices. Workspaces save and restore that same list, in a form that survives
devices being added or removed between versions: see [Saving and loading](#saving-and-loading).

**The frontend reads the bus list.** It builds its menus and dialogs from it, and doesn't need to
know what is behind a slot.

**The frontend acts through methods.** Slots that take devices while the machine runs have
emulator commands to create and remove them. Functions have methods to configure a device:
loading a disk image, choosing the network, and so on.

## Terms

- **Bus:** a list of slots. Some take devices while the machine runs (SCSI), and some hold what
  the machine is built with (NuBus, built-in devices). Buses are one flat list: a card that adds a
  bus puts it in that list, identified by its own id.
- **Slot:** an id, an optional name, and maybe a device.
- **Device model:** the implementation of one kind of device, such as `ScsiHardDisk`. It provides
  one function and fits one bus type. A **device** is one instance of it in a slot: it owns all its
  internal state, and can have attributes of its own, such as a ROM.
- **Function:** what the device does for the user, such as HardDisk or Ethernet, and its interface
  to the frontend. It's a trait with no data, whose methods act on the device, such as loading a
  disk image. It comes with a status struct, which the device fills in.
- **Slot trait:** how a bus's controller talks to the device in a slot. A SCSI device takes SCSI
  commands; a NuBus card answers reads and writes and raises an interrupt. It's independent of the
  function: a NuBus card and a SCSI device can both provide Ethernet.
- **Mac model:** the machine, `MacModel`. This document never says "model" alone.

Machine settings aren't devices: the main ROM, the RAM size, PRAM, the mouse mode and the shared
folder stay where they are.

## Structures

The core code is in `core/src/emulator/devices.rs`, with each device model's record next to its
implementation.

### Bus and slot

```rust
pub enum BusKind { Swim, Scsi, Nubus, LcPds, Se30Pds, IieDisk, Serial, BuiltIn }

pub struct Bus {
    pub kind: BusKind,
    pub id: String,           // "SCSI", "Floppy:PDS_LC.0". In the workspace files: must not change.
    pub title: String,        // In the menus: free to change
    pub slots: Vec<Slot>,
}

pub struct Slot {
    pub id: String,           // "3", "9", "A"
    pub name: Option<String>, // "internal", "Modem"
    pub hint: Option<String>, // What the guest needs to use this slot, for the menus to say
    pub device: Option<Device>,
}
```

The list has no hierarchy, and a bus has no parent field. A bus that a card provides says so in its
`id`: the SCSI bus of a card in NuBus slot A would be `SCSI:NuBus.A`.

### Device

```rust
pub struct Device {
    pub model: DeviceModel,
    pub settings: serde_json::Map<String, Value>,  // The device model's own attributes, such as a ROM
}

pub enum DeviceModel {
    ScsiHardDisk, Hd20, ScsiCdrom, DaynaPortScsiLink, LaserWriterIISC,
    Mac400KDrive, Mac800KDrive, Mac800KPwmDrive, MacSuperDrive,
    UniDisk35, Apple525Drive,
    DisplayCard824, MacIIVideoCard, SE30Video,
    AppleIIePdsCard,
    LocalTalkAdapter, SerialBridge,
}

pub enum FunctionKind {
    HardDisk, Cdrom, Floppy, Ethernet, Printer, Video, AppleIIe, SerialLink, LocalTalk,
}
```

Device models behind a disabled feature, such as `DaynaPortScsiLink` without `ethernet`, aren't in
the enum on that build. A device model's name in the workspace files is its variant's name.

Each device model has one static record, in its own source file. The frontend reads it to know
about a device model before any device of it exists:

```rust
pub struct ModelInfo {
    pub model: DeviceModel,
    pub name: &'static str,                   // In the menus: free to change
    pub bus: BusKind,                         // The bus type it fits, so the slot trait it implements
    pub fits: fn(MacModel) -> bool,           // The Mac models it fits, among those with that bus
    pub function: FunctionKind,
    pub fixed: bool,                          // Created with the machine, never added or removed
    pub one_per_bus: bool,                    // A second one on the same bus is refused, and greyed
    pub rom: Option<RomInfo>,                 // The ROM file it needs, if any, and how to check it
    pub path_keys: &'static [&'static str],   // The settings that are paths: ["media"], ["rom"]
    pub adds_buses: &'static [BusKind],       // The buses a device of this model adds
    pub monitors: &'static [Option<MacMonitor>], // A video card's monitors, None for nothing
    pub(crate) create: Option<CreateFn>,      // How a device of it is made. None: by the machine build
}

pub struct RomInfo {
    pub label: &'static str,                  // "Display Card 8-24 ROM"
    pub digests: &'static [&'static str],     // SHA-256 of each ROM known to work
}

/// How a device model's file makes a device. One kind per bus type, because each bus holds its
/// devices behind its own slot trait.
pub(crate) enum CreateFn {
    Scsi(fn(&DeviceConfig, &mut CreateContext) -> Result<Box<dyn ScsiTarget>>),
    Hd20(fn(&DeviceConfig) -> Result<DcdDevice>),
    Serial(fn(&DeviceConfig) -> Result<SerialBridgeConfig>),   // The host link a serial device opens
}

/// What making a device can draw on, besides its settings
pub(crate) struct CreateContext<'a> {
    pub(crate) audio: Option<&'a mut dyn AudioProvider>,       // Where a CD-ROM drive plays audio
}

/// One line per device model, with its feature gate beside it
pub static MODELS: &[&ModelInfo] = &[
    &crate::mac::scsi::disk::INFO,
    &crate::mac::swim::dcd::INFO,
    &crate::mac::scsi::cdrom::INFO,
    #[cfg(feature = "ethernet")]
    &crate::mac::scsi::ethernet::INFO,
    // ...
];

/// What a slot on this bus can take, on this Mac model
pub fn models_for(bus: BusKind, mac: MacModel) -> impl Iterator<Item = &'static ModelInfo>;
```

Rust has no `initcall`, so a file can't register itself at startup: one table lists the records,
a line each. What a slot can take is `models_for(bus.kind, mac)`: the frontend's menus and dialogs
list it, and the core refuses anything else. The device model checks its ROM against its record's
digests, unless the user turns ROM validation off.

A record that isn't `fixed` has a create function, which makes a device from its entry. Creating a
device is a call through its record, so `devices.rs` has no `match` on device models to do it.

| Device model | Function | `fixed` |
| --- | --- | --- |
| `ScsiHardDisk`, `Hd20` | HardDisk | No |
| `ScsiCdrom` | Cdrom | No |
| `Mac400KDrive`, `Mac800KDrive`, `Mac800KPwmDrive`, `MacSuperDrive` | Floppy | Yes |
| `UniDisk35`, `Apple525Drive` | Floppy | Yes, the card builds its chain from them |
| `DaynaPortScsiLink` | Ethernet | No |
| `LaserWriterIISC` | Printer | No |
| `DisplayCard824`, `MacIIVideoCard`, `SE30Video` | Video | Yes |
| `AppleIIePdsCard` | AppleIIe | Yes |
| `SerialBridge` | SerialLink | No |
| `LocalTalkAdapter` | LocalTalk | No |

Whether a device model is `fixed` decides what the menus offer and what can be removed, so `Bus`
needs no flag for it:

| Where | What a slot can take | What can be removed |
| --- | --- | --- |
| Devices menu, machine running | Device models that aren't `fixed`, on empty slots | Devices that aren't `fixed` |
| Load ROM dialog | `fixed` device models | Not applicable: it builds a new machine |

**A device model is the device the guest sees; an attribute is how the host backs it, or a revision
of that device.** SCSI hard disks and HD20s are different device models with the same function. A
read-only HD20 and a writable one are the same device model. A CD-ROM drive with an image and one
with a Toolbox managed folder of images are the same device model, with `media` a file or a
folder.

### Functions and their status

A function is the interface from the frontend to a device: a trait whose methods act on the device.
The function holds no data itself.

Each function comes with a status: a struct that tells the frontend how that function stands, such
as a floppy drive's disk title, or whether its disk has unsaved writes. The device keeps it up to
date when asked, and the frontend gets a copy whenever it changes, as
[Keeping the frontend up to date](#keeping-the-frontend-up-to-date) explains. The status also
carries the function's attributes in its `settings` member, such as the image path and
`read_only`: that's what a workspace saves.

Status isn't state. A device owns its state, such as a floppy drive's head position, and never
uses the status to hold it: it only writes to the status when asked. Save states store the state
through serde. A device can also implement `Debuggable` to show its internal state in the
debugger, which draws it without knowing the device model.

```rust
pub trait Floppy {
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool;  // True if anything changed
    fn load(&mut self, media: &Path, read_only: bool) -> Result<()>;
    fn eject(&mut self);
}

pub trait HardDisk {
    fn update_status(&mut self, status: &mut HardDiskStatus) -> bool;
    fn branch(&mut self, path: &Path) -> Result<()>;   // Carries on from a copy of the image
}

pub trait Cdrom {
    fn update_status(&mut self, status: &mut CdromStatus) -> bool;
    fn load(&mut self, path: &Path) -> Result<()>;
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()>;  // An image already in memory
}

pub trait Ethernet {
    fn update_status(&mut self, status: &mut EthernetStatus) -> bool;
    fn link(&self) -> EthernetLinkType;
    fn set_link(&mut self, link: EthernetLinkType) -> Result<()>;
    fn start_capture(&mut self, filename: &Path) -> Result<()>;
    fn stop_capture(&mut self) -> Option<(PathBuf, usize)>;
    fn capture_status(&self) -> Option<EthernetCaptureStatus>;
}

pub enum FunctionStatus {
    HardDisk(HardDiskStatus),
    Cdrom(CdromStatus),
    Floppy(FloppyStatus),
    Ethernet(EthernetStatus),
    Printer,
    Video,
    AppleIIe,
    SerialLink(Option<SerialBridgeStatus>),
    LocalTalk(Option<SerialBridgeStatus>),
}
```

Printer, Video and AppleIIe have nothing for the frontend to do yet, so they have no trait, and
serial devices are reached through their slot trait, `SerialDevice`. The emulator only sees a device
through its function:

```rust
pub enum FunctionMut<'a> {
    HardDisk(&'a mut dyn HardDisk),
    Cdrom(&'a mut dyn Cdrom),
    Floppy(&'a mut dyn Floppy),
    Ethernet(&'a mut dyn Ethernet),
    SerialLink(&'a dyn SerialDevice),
    LocalTalk(&'a dyn SerialDevice),
    Printer, Video, AppleIIe,                 // Nothing for the frontend to do yet
}
```

A Mac drive and an Apple IIe Card drive implement the same `Floppy`, so the menu has one code path
and names no device model. Their image formats and internals stay different behind the trait.
What a drive can't do at all, such as a blank 1.44 MB disk in a 5.25" drive, is hidden, and what it
can't do right now is greyed.

Each device model has one settings struct: what its function needs, such as a disk image, plus the
attributes of that device. So a NuBus card's struct has `rom` and a SCSI disk's doesn't.

| Device model | Settings |
| --- | --- |
| `ScsiHardDisk` | `media` |
| `Hd20` | `media`, `read_only` |
| `ScsiCdrom` | `media` (optional): an image, or a folder of images |
| `MacSuperDrive` and the other drives | `media` (optional), `read_only` |
| `DaynaPortScsiLink` | `link` |
| `DisplayCard824` | `rom`, `monitor` |
| `MacIIVideoCard` | `rom` |
| `SE30Video` | `rom` |
| `AppleIIePdsCard` | the card's settings: jumper, drive chain, joystick |
| `SerialBridge` | `tcp_port` (optional) |

A card that holds a bus would provide a `Card` function, and its device model would list the buses
it adds. A device that does two things, such as a video and Ethernet card, is a card that adds a
bus with two devices on it, so each device still has one function.

### Slot traits

A bus type's slot trait is how its controller talks to the device in a slot. The function faces the
frontend; the slot trait faces the bus.

| Bus type | Slot trait | What the device answers |
| --- | --- | --- |
| SCSI | `ScsiTarget` | SCSI commands |
| NuBus | `NubusSlot` | Reads and writes in its slot, and an interrupt |
| Serial | `SerialDevice` | Bytes or frames from the SCC |

A slot trait also hands out the device as its function. `ScsiTarget` is only the SCSI side,
commands and blocks, plus `function()`, which returns the target as the function it provides. So
the emulator reaches the function of any SCSI device without knowing its device model.

A renderer isn't part of any slot trait: a video card owns one, as it owns its VRAM, and whoever
creates the card gives it one. Built-in video works the same way.

## NuBus

### Cards take whole transactions

A card answers whole NuBus transactions, a byte, a halfword or a word, through its slot trait:

```rust
pub trait NubusSlot: Tickable + Debuggable + Display {
    /// One NuBus read: `size` bytes (1, 2 or 4) at `addr`, aligned to the size, right-justified
    /// as the 68k sees them. `addr` has the digit of the slot cleared: $F0xx_xxxx or $0xxx_xxxx.
    /// None: the card doesn't answer at that address.
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32>;
    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()>;
    fn get_irq(&self) -> bool;
    fn reset(&mut self);
}

pub trait VideoCard<TRenderer> {            // Video cards only
    fn blank(&mut self) -> Result<()>;
    fn render(&mut self) -> Result<()>;
    fn set_renderer(&mut self, renderer: TRenderer);
}
```

`nubus::card_access()` makes a CPU cycle into NuBus transactions, as the NuChip of the Mac II,
IIx and IIcx does (Designing Cards and Drivers for the Macintosh II and SE, ch. 3, "Non-aligned
MC68020 accesses"): a read cycle is answered as 32-bit, and a nonaligned one reads the whole word;
a write cycle that isn't a transaction is answered as a 16-bit port, so the CPU makes the pieces
itself. A 68040 only makes cycles that are transactions already, which go through unchanged. The
wait states stay in the bus helpers, around the access.

Only video cards have a renderer and `VideoCard`, so a card without video needs no stub methods.
Cards stay in an enum, `NubusCard`, so an access is a `match`, not a virtual call. The SE/30's video
isn't a card: it's hard-wired, so it's a plain field of the bus, in the `Built-in` bus of the device
list, and keeps its quirks in its own code.

### Standard and super slot spaces

Each slot has two address spaces: its standard slot space, `$Fsxx_xxxx`, 16 MB, and its super slot
space, `$sxxx_xxxx`, 256 MB, where `s` is the slot number (Designing Cards and Drivers for the
Macintosh Family, 3rd ed., ch. 3 and 7). No NuBus signal tells them apart: a card compares the
address with the ID of its slot, either bits 27-24 when bits 31-28 are all ones, or bits 31-28.

The bus only gives a card the addresses of its own slot, in either space, with the digit of the slot
cleared (`nubus::card_address()`): `$F0xx_xxxx` in its standard slot space and `$0xxx_xxxx` in its
super slot space. So a card still tells the two spaces apart, as a real one does, and its address map
doesn't depend on the slot it's in. `nubus::standard_offset()` gives the offset in the standard slot
space, and None in the super slot space, which is what every card in Snow refuses today. A card that
needs its super slot space only has to answer there.

What no card answers in super slot space stays as each machine had it: open bus on the Mac II
family and the Quadra 700, and a bus error on the Quadra 800. A standard slot that nothing answers
is a bus error, as the Slot Manager expects of an empty slot.

### The cards of a machine

`nubus::Nubus` holds the cards of slots `$9` to `$E` for every machine with NuBus slots. It routes a
cycle in either address space of a slot to the card there, ticks the cards, gives their interrupts
one bit per slot (bit n for slot `$9 + n`, as VIA 2 port A has them on every machine), makes the
cards from their configuration, and lists their displays.

| Mac model | Slots | Slot interrupts | Source |
| --- | --- | --- | --- |
| Mac II, IIx | `$9` to `$E` | VIA 2 PA0-PA5 | Guide to the Macintosh Family Hardware, 2nd ed., table 4-9 |
| IIcx | `$9` to `$B` | VIA 2 PA0-PA2 | Same table |
| Quadra 700 | `$D`, `$E` (the PDS shares `$E`) | VIA 2 PA4, PA5 | Designing Cards and Drivers, 3rd ed., table 16-6 |
| Quadra 800 | `$C` to `$E` (the PDS shares `$E`) | VIA 2 PA3-PA5 | As MAME has them; IOSB holds the VIAs |

The slot interrupts, and the built-in video's on the Quadras, combine into VIA 2 CA1. The wait
states of a transaction on the Quadras are estimates, for the 10 MHz NuBus and the crossing to the
CPU's clock. The NuBus controllers of the Quadras, the YANCC and Kiwi, take a write in a buffer and
let the CPU go on, which isn't modelled.

The PDS slots of the Quadras aren't there yet. A PDS card that uses slot `$E` tells the NuBus
controller with `/PDS.SLOT.E.EN`, which then leaves `$Exxx_xxxx` and `$FExx_xxxx` to it.

## How they interact

The emulator owns the real objects, and describes them to everyone else with `Bus`, `Device` and
the status structs; everything coming back in is an emulator command.

```
 frontend thread                         emulator thread
 ----------------------------------      -------------------------------------
 Workspace file  --load-->  Frontend  --commands-->  Emulator    --methods-->  Real objects
 (flat devices   <--save--  (menus,   <--bus list--  (devices.rs) <--status--  (SCSI disk, HD20,
  list)                      dialogs)  <--status----                            floppy drives)
```

The emulator runs on its own thread, so the frontend can't call a trait directly. It acts through
**emulator commands**: messages it sends the emulator thread over a channel, the variants of
`EmulatorCommand`, such as `AddDevice(config)`, `RemoveDevice(addr)` or
`LoadMedia(addr, path, read_only)`. They aren't the commands a device on a bus gets, such as SCSI
commands. The emulator thread runs them between batches of guest instructions, and `devices.rs`
turns each into the call.

An emulator command names a slot by a `SlotAddr`: the index of its bus in the bus list, and of the
slot in that bus. It's never saved, because the indices depend on the Mac model; workspaces and save
states use the bus and slot ids. It stays valid while the machine runs, because the list's shape
doesn't change then. Which device is at an address can change, though: removing an HD20 moves the
ones behind it up.

| Task | How |
| --- | --- |
| Menu shows a slot | Match on the slot's status; no device model is named |
| Menu lists what a slot can take | `models_for(bus.kind, mac)`, with each record's `name` and `function` |
| User attaches a device | `AddDevice(config)` |
| User removes a device | `RemoveDevice(addr)` |
| User changes something | An emulator command per method, such as `LoadMedia(addr, path, read_only)` |
| Core handles an emulator command | Slot → real object → function trait → method |
| Core reports | Gives each device its turn with `update_status()` |

### Keeping the frontend up to date

The bus list and the status take separate routes, and neither does any work when nothing has
changed.

**The bus list** is built when the machine is created, and changes only when a device is added or
removed. A media change doesn't touch it, because the image path is in the status. The frontend
holds it as an `Arc<Vec<Bus>>`, so it reads the list with no lock and no copy. On a change the
emulator builds a new list and sends the pointer over the channel.

**The status** of every device sits in one block, indexed by `SlotAddr`:

```rust
pub struct StatusBlock {
    pub generation: AtomicU64,
    pub statuses: Mutex<Vec<Vec<Option<FunctionStatus>>>>,
}
```

The emulator and the frontend share it through an `Arc`. Devices never see the lock or the `Arc`:
they only get a `&mut` to their own status, in a pass that runs every 500 ms and after most
emulator commands. The pass takes the lock with `try_lock()`, so the emulator never waits on the
UI: a skipped pass loses nothing, because a device's own change flag stays set until it's asked.
When any device returns `true`, the generation goes up. Adding or removing a device bumps it
whatever the devices return, so a new device's first status shows.

The frontend reads `generation` each frame, with no lock. If it changed, it takes the lock, copies
the whole block and draws from its copy. So the menus never touch the lock, and each side takes it
at most about twice a second. A change the guest causes, such as a software eject, shows up to
500 ms late.

Save states are unaffected, because devices hold no handle to the block. A restored machine gets a
new block, and the next pass fills it.

### Creating a device from the menu

A bus whose slots take devices while the machine runs lets the frontend create them, by passing a
`DeviceConfig` built from the user's answers. The device models registered for the slot's bus type
say what can be created, and the function tells the frontend what to ask: an image for a hard
disk, nothing or an image or a folder for a CD-ROM drive, nothing for an Ethernet adapter. The
frontend never names a device model to do this, so a new device model of an existing function only
touches the core: its file, with its record, its create function and its trait implementations,
and its line in the table. A new function needs frontend code, to ask and show what it needs.

Changing media on an existing device is a function method, through `LoadMedia(addr, path)`.
`AddDevice` with media only creates the device with something already in it.

## How a machine is created

The frontend builds one list of `DeviceConfig`, however the machine comes to exist, and the core
handles every entry the same way: if the device exists it gets the settings, if not the slot
creates it, and if neither works the entry is refused with a reason.

`DeviceConfig` is one entry, and also exactly what the workspace file stores:

```rust
pub struct DeviceConfig {
    pub bus: String,
    pub slot: String,
    #[serde(rename = "device")]
    pub model: Option<String>,   // None: the device the machine has in this slot
    #[serde(flatten)]
    pub settings: serde_json::Map<String, Value>,
}
```

The settings are a dictionary, an in-memory form of the JSON. So a workspace passes through code
that doesn't understand it, and an entry for a device model this build doesn't know can still be
carried and kept. Code that knows a device model uses its typed struct, owned by the core:

```rust
pub trait DeviceSettings: Serialize + DeserializeOwned { const MODEL: DeviceModel; }

let devices = vec![
    DeviceConfig::new("NuBus", "9", &DisplayCard824Settings { rom, monitor })?,
    DeviceConfig::new("SCSI", "0", &ScsiHardDiskSettings { disk })?,
];
let (emulator, displays, refused) = Emulator::new_with_devices(&machine, devices)?;
```

The struct names its device model, so the frontend can't pair settings with the wrong one, and a
mistyped field is a compile error. `machine`, a `MachineConfig`, holds what isn't a device: the main
ROM, the Mac model, the RAM size, the 68882 and PMMU options and the built-in video's monitor.

| The list comes from | How |
| --- | --- |
| Workspace load | The file's entries, with old fields converted and paths made absolute |
| Load ROM dialog | One entry per card row, plus the devices of the running machine |
| Restart | `emu.devices()` plus what was refused, in the same form |

Empty slots, default floppy drives and implicit controllers need no entry, so a Plus with nothing
attached has an empty list.

### Inside `new_with_devices`

1. **Before the build,** take the entries for `fixed` device models. A card entry makes its card,
   including what the card builds from, such as the Apple IIe Card's drives. A drive other than the
   Mac model's default changes that drive. An entry that names no device means what the Mac
   model's layout already has in that slot.
2. **Build the machine** with the per-Mac-model code, with its cards.
3. **Apply every remaining entry,** in one pass. A card's bus is already there, because the card
   was made with the machine.
4. **Return the machine, its displays and what was refused.**

| Case | What happens |
| --- | --- |
| The device exists, such as a built-in floppy drive | It gets the settings, for example `media` |
| The slot is empty and takes that device model | The device is created from the entry |
| The slot can't take it, the device model is unknown, or `device` is missing | Refused, entry kept |
| No such slot on this machine | Refused, entry kept |
| The device can't use a setting, such as a bad path | The device stays, refused, entry kept |

What was refused is one toast, and each entry is also in the log. Each device model has one
creation path: the create function in its record for the ones that aren't `fixed`, and the machine
build for the ones that are. The source of the list makes no difference to the code that runs.

### While the machine runs

- **Attaching from a menu** builds a `DeviceConfig` from the user's answers and sends
  `AddDevice(config)`, which reaches the same creation code.
- **Removing** sends `RemoveDevice(addr)`, for devices that aren't `fixed`.
- **Configuring** uses one emulator command per function method.

### Reporting

A device's function attributes, such as `media`, are in its status, and its own attributes, such
as a ROM, are in the bus list. The machine reports `devices()`, the same `DeviceConfig` list it can
be built from. What a machine reports is what can create it again, which gives one test per Mac
model family: report a machine, build a new one from that, and report again. The result must be
the same.

## Where buses come from

The initial list belongs in `MacModel`, because the built-in buses are facts about a Mac model; the
emulator adds the devices and the buses that cards bring.

```rust
impl MacModel {
    /// The buses every machine of this model has, with its fixed devices in their slots
    pub fn buses(self) -> Vec<Bus>;
}
```

It can be read before a machine exists, which is needed to check a workspace's card slots.

| Bus id | Comes from | Slots |
| --- | --- | --- |
| `SWIM` | `fdd_drives()` | `1` upwards for drives, named internal or external; `HD20.1` to `HD20.4` |
| `SCSI` | `has_scsi()` | `0` to `6` |
| `NuBus` | `nubus_slots()` | `9` to `E`, as the Mac model has them |
| `PDS_LC` | The LC family | `0` |
| `PDS_SE30` | The SE/30 | `0` |
| `Serial` | Every Mac model | `A` Modem, `B` Printer |
| `Built-in` | Mac models with devices on the logic board and no real bus behind them, such as the SE/30's video | `0` upwards |

The emulator starts from `model.buses()`, adds what's in each slot, and appends the buses its cards
add, such as `Floppy:PDS_LC.0` for the Apple IIe Card's drives. A SCSI card in NuBus slot A would
add `SCSI:NuBus.A`, and the second channel of a dual-channel card `SCSI.B:NuBus.A`.

### PDS slot types

PDS slots come in incompatible types, so each type is its own `BusKind`, with its own bus id and
device models: `LcPds` on the LC family and `Se30Pds` on the SE/30. `AppleIIePdsCard` registers
only for `LcPds`, so the dialog can't put it in an SE/30, and a workspace entry for `PDS_LC` opened
on an SE/30 finds no such bus and is kept.

### Floppy drives and HD20s

Floppy drives and HD20s share one bus, because they're on one controller and an HD20 uses the same
connector as the external drive. The id stays `SWIM` on every Mac model, even with an IWM, so a
workspace can move between Mac models.

| Slot | Holds | `fixed` |
| --- | --- | --- |
| `1` upwards | `Mac800KDrive` and the like, with optional media | Yes |
| `HD20.1` to `HD20.4` | `Hd20`, or nothing | No |

- **An empty drive and an empty slot differ:** a drive with no disk is a device with no `media`. A
  Mac model without an external drive has no slot for it.
- **Floppy drives can't be removed,** only their media changes. The drive's device model is chosen
  in the Load ROM dialog.
- **An HD20 only attaches at the next free position,** so the frontend greys the later slots. The
  guest only sees it after a restart, which the slot's hint says.
- **Removing an HD20 moves the ones behind it up,** so an HD20's slot id is its position on the
  chain, not a fixed place for that device: a workspace or save state written afterwards has the new
  ids.
- **An HD20 provides HardDisk,** like `ScsiHardDisk`, so it uses the same dialog and menu code.

Creating a machine takes these in three steps: the drives' device models before the build, then the
disks, then the HD20s in slot order, each at the next free position. If `HD20.1`'s image is missing,
`HD20.2` still attaches, at position 1. On save, the kept entry for the missing image is numbered
after the live HD20s, so a live device doesn't displace it.

### Serial devices

Each serial port is a slot on the `Serial` bus: `A` Modem and `B` Printer. What goes in it talks to
the SCC in one of two ways, as the guest sets the SCC's mode:

| Device model | Talks to the SCC in | Function |
| --- | --- | --- |
| `SerialBridge` | Bytes | SerialLink: a PTY or a TCP port on the host |
| `LocalTalkAdapter` | LLAP frames | LocalTalk |

The serial bus's slot trait, `SerialDevice`, has both byte and frame calls, and a device ignores the
ones it doesn't support. If the guest's mode doesn't suit the device, nothing gets through, as with
a LocalTalk dongle under a terminal program.

### From a slot to the real object

A slot resolves to a target, and only `devices.rs` knows what that means:

```rust
enum SlotTarget {
    Drive(usize),           // swim.drives[i]
    Hd20(usize),            // A position on the HD20 chain
    Scsi(usize),            // The SCSI bus's targets[id]
    Card,                   // A card, made with the machine
    IieDrive(usize),        // The Apple IIe Card's chain
    Serial(SccCh),
}
```

## GUI

The core only describes buses, slots and devices, so the GUI can present them differently with no
change to the core or the workspace file. The frontend walks the flat list of buses and groups them
for the user; `BusKind` decides the grouping, and what a slot can take comes from the device models
registered for that bus type.

### Devices menu

- **SCSI:** each bus has its own title and slots.
- **Floppies:** internal and external are one controller, so one bus, told apart by the slot's
  `name`. The Apple IIe Card's drives are their own bus and group.
- **Cards** are chosen in the Load ROM dialog, because changing one means building the machine
  again.

The device models a bus takes become menu items by function: "Attach Ethernet" for one device
model, a submenu with each `name` for several, greyed when the device model is one per bus and the
bus has one. Slots without a menu, such as those of the video cards, aren't shown.

### Load ROM dialog

The dialog that chooses the machine also chooses its cards, with a row per slot that takes them,
in its "Select peripherals" section, under the monitor of the built-in video if the machine has
one. The "Select ROM files" section only has the ROMs of the machine: the main ROM, and the SE/30's
video ROM.

```
Macintosh model:   [Macintosh II          v]

Select ROM files
System ROM:        [macii.rom      ] [Browse]

Select peripherals
NuBus slot 9       [Display Card 8-24     v]
    ROM            [8-24.rom       ] [Browse]
    Monitor        [14" high-res          v]
NuBus slot A       [Empty                 v]
...
NuBus slot E       [Empty                 v]
```

- **Rows follow the Mac model:** a Mac II gets six NuBus rows, a IIcx three, a Quadra 700 two and a
  Quadra 800 three.
- **Each menu** lists "Empty" and the device models registered for that bus type, by `name`.
- **A required card:** a Mac II needs a video card, so the dialog won't start the machine without
  one and says why. The Quadras have their built-in video and need none.
- **Fixed devices get no menu:** the SE/30's built-in video only shows its ROM row.
- **A card with a ROM gets a ROM row below it.** ROMs are remembered per card. Whether a file is the
  right ROM comes from the card's record.
- **The monitors of a card** come from its record's `monitors`. None leaves its connector empty,
  which its sense lines report as nothing connected (Technical Note HW30): such a card has no
  display.
- **More than one display** brings a red warning: several displays are an experimental feature
  known to have issues. See [Several displays](#several-displays).

The dialog's result is a list of devices, in the same form as a workspace's entries, so the machine
is created through the same path as a workspace load.

A card's ROM row is generic, from its record. Anything else a card needs asking, such as the 8-24's
monitor or the Apple IIe Card's options, is added under its row by code for that card: this is the
one place the GUI names device models.

## Several displays

**This is experimental, and known to have issues.**

A machine gives one display per screen, each with a name and the place its frames arrive: the
built-in video first, then one per video card with a monitor, in NuBus slot order. A restored state
makes them again. With several displays and the absolute mouse, the emulator reads where Mac OS puts
each screen, from Color QuickDraw's device list, once a second. It knows which display a screen is
from the slot of the base address its driver was given (dCtlDevBase), not from the slot the driver
reports: the driver of the Quadras' built-in video reports slot 0, while the video answers in the
space of slot `$9`. The GUI draws the displays in that
arrangement, fitted to the window, and a position on any screen the guest uses becomes a global
position for the mouse. A screen the guest doesn't use yet, whose coordinates mean nothing, and a
display that isn't in the list at all, as under the ROM, whose QuickDraw only has the startup
screen, go to the right of the others, where they start, and take no mouse input. A row holds three
displays at most, the guest's screens counting in the first, and the next ones go in a row under
it. In relative mouse mode the guest's memory is left alone, as it may not be running
Mac OS.

Known issues, to look at:

- Moving screens in the Monitors control panel: the arrangement follows at first, then stops
  following.
- The absolute mouse is one event behind: the GUI takes the pointer position of the previous frame.
- The cursor lags more with several displays.
- Inspecting memory on a Mac II ignores the AMU, so pointers in 24-bit mode are followed as
  StripAddress does; inspecting through the AMU would remove that.
- The mouse mode is read from each machine's bus; it should move to a common place.
- With a guest that isn't Mac OS, such as A/UX or Linux, nothing tells where the displays are: they
  could be arranged by hand.

## Saving and loading

### Entries

The workspace stores the flat list of `DeviceConfig`, the same list a machine is created from: the
bus id, the slot id, the device model's name, and its settings. A Mac II with a video card, a disk,
an empty CD-ROM drive, a floppy in the drive and two serial devices:

```json
{
  "model": "MacII",
  "rom_path": "macii.rom",
  "devices": [
    { "bus": "NuBus",  "slot": "9", "device": "DisplayCard824", "rom": "8-24.rom" },
    { "bus": "SCSI",   "slot": "0", "device": "ScsiHardDisk", "media": "a.img" },
    { "bus": "SCSI",   "slot": "3", "device": "ScsiCdrom" },
    { "bus": "SWIM",   "slot": "1", "media": "system.img" },
    { "bus": "Serial", "slot": "A", "device": "SerialBridge", "tcp_port": 5000 },
    { "bus": "Serial", "slot": "B", "device": "LocalTalkAdapter" }
  ]
}
```

The `SWIM` entry has no `device`, so it means the drive already in that slot, whichever drive the Mac
model has. That lets a workspace move between Mac models. An entry only names a drive's device
model if it differs from the default, such as the 800K option on a 128K. A slot that takes devices
while the machine runs always needs `device`. Any device the Mac model has in a slot works the same,
such as the SE/30's video with its `rom`.

```json
{ "bus": "SWIM",            "slot": "HD20.1", "device": "Hd20", "media": "a.img", "read_only": true }
{ "bus": "SCSI",            "slot": "3", "device": "ScsiCdrom", "media": "cd-images/" }
{ "bus": "SCSI",            "slot": "4", "device": "DaynaPortScsiLink", "link": "NAT" }
{ "bus": "PDS_LC",          "slot": "0", "device": "AppleIIePdsCard" }
{ "bus": "Floppy:PDS_LC.0", "slot": "1", "device": "UniDisk35", "media": "prodos.po" }
```

Bus ids and device model names are the file format and must not change. Menu text comes from
`title` and `name`, which can change freely. `SlotAddr` is never saved, so adding, removing or
reordering buses changes nothing in a file.

### Loading

1. **Parse** the file into `DeviceConfig`. An entry missing `bus` or `slot` is discarded.
2. **Convert old fields** into entries.
3. **Make paths absolute,** using the record's `path_keys` for device models this build knows.
4. **Decide the Mac model.** If it's unknown or the main ROM is missing, open the Load ROM dialog.
5. **Create the machine** with the list, as [How a machine is created](#how-a-machine-is-created)
   describes. Refused entries are kept.
6. **Warn once,** with one toast listing everything that wasn't used.

A load that fails returns early and leaves the running machine alone.

### Saving

1. Write `emu.devices()`, leaving out fixed slots that hold nothing and default drives with no
   media. What's default comes from comparing each device with a fresh `model.buses()`.
2. Add the kept entries whose slot isn't occupied now. A live device always wins, except that a
   kept HD20 entry is numbered after the live ones.

Changing the Mac model in the dialog follows the same flow, so SCSI disks are kept while on a
machine without SCSI, and come back on the way back. In memory the list holds absolute paths; on
load and save the paths named by `path_keys` are rebased against the workspace file.

### What is forgiven

| In the file | Result |
| --- | --- |
| Unknown device model, or one not enabled on this build | Entry kept, warning |
| No such bus or slot on this machine | Entry kept, warning |
| Bus doesn't take that device model | Entry kept, warning |
| Hard disk image or card ROM missing | Entry kept, slot empty, warning |
| Floppy or CD image missing | Drive stays, empty; the entry's `media` is kept, warning |
| Unknown attribute | Ignored |
| Unknown `model`, missing main ROM | Load ROM dialog |
| Broken JSON, wrong scalar type | The load fails: the file is damaged |

Once the format has shipped, a new attribute must be safe for an older build to ignore. If it
isn't, it needs a new device model, because an unknown device model is always kept and unused.

### Converting old files

Old fields are read and never written.

| Old field | Becomes |
| --- | --- |
| `disks`, `scsi_targets` | `SCSI` entries |
| `video_card`, `display_card_rom_path` | A `NuBus` entry in slot `9` on the Mac II family; a `Built-in` entry on the SE/30 |
| `serial_bridges` | `Serial` entries |
| `floppy_images` | A `SWIM` entry with `media` |
| `override_fdd_type` | `SWIM` entries naming the drive's device model |

### Save states

A save state holds the machine as serde has it, and the devices of the slots that change while the
machine runs, by bus and slot id, as a workspace does. Fixed devices, such as floppy drives and
cards, stay in the machine's state, because the machine is built with them. The file, version 3,
is:

- The header, with the device list as JSON.
- The machine's state, without the devices of the changing slots: the SCSI targets and the HD20s
  aren't serialized with it.
- A list of slot states, one per device of a changing slot.
- The media of those devices, one after the other, such as a disk's image.

```rust
struct SlotState {
    bus: String,
    slot: String,
    state: Vec<u8>,          // The device, serialized by itself
    media_len: Option<u64>,  // The size of its media, which follows the list
}
```

Restoring builds the machine from its state, then puts each saved device back in the slot its ids
name: the device is deserialized, gets its media, which the restore writes to a temporary file, and
the slot takes it. A disk runs on that copy but still reports the image the user gave, so a
workspace saved afterwards has the original path. A CD-ROM drive opens its image again, and a
DaynaPORT its network link. Serial links aren't in the state: they're created again from the device
list.

## Limits and open points

### Limits

- **A device that adds a bus is fixed for the life of the machine.** Changing one means a restart,
  as with real cards. This keeps the list's shape constant, so `SlotAddr` stays valid.
- **Moving a card orphans what's on its bus,** because the bus id contains the slot. The entries are
  kept with a warning.
- **The format is one-way.** Older versions of Snow ignore `devices`.
- **A mismatch between what's saved and what's created only shows at run time,** because settings
  travel as a dictionary. The round-trip test per Mac model family is what catches it.
- **Kept entries for unknown device models can't have their paths rebased,** because `path_keys`
  needs the device model. This only matters when saving to another folder.
- **The frontend picks the drive for a dropped disk.** Its MOOF and writeback prompts need to know
  which drive, so it takes the first free one from its copy of the status.

### Open questions

- **HD20s on Macs other than the compacts?** Their ROMs don't look for HD20s, and only Apple's
  MacIIHD20 INIT makes them work. They could be offered only on the compacts, or behind a checkbox.
  For now the frontend hides the empty HD20 slots after the Plus.
- **Removing an HD20 from the middle of the chain?** Today the ones behind it move up, which renames
  them. Letting only the last one be removed, as on the real chain, keeps every slot id fixed for
  its device's life.
- **The HD20 slots' menu labels** read "Floppy #HD20.1 HD20".
- **Can `MacModel` describe the Load ROM dialog?** It could give the layout with nothing plugged in,
  and the dialog would fill itself from that. The frontend would still need to know some settings,
  such as ROM matching and the 68882 checkbox. For now the frontend knows.

### Later

- The PDS slots of the Quadras, and NuBus cards that aren't video.
- The issues of [Several displays](#several-displays).
- The LocalTalk network, with its UDP link and the AFP server as participants; which network the
  AFP server joins, and network stacks with several Ethernet adapters.
