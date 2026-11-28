# The device model

## Overview

The devices of an emulated Mac have three main sets of interfaces:

1. **With the emulated Mac.** This part is represented by buses, slots and the devices in them.
   Each type of bus has a *slot trait*, which its devices implement: the API the Mac uses to talk
   to a device on that bus. A SCSI device receives commands (CDBs) and returns their data and
   status; a NuBus card is read and written, and raises an interrupt. In this part of the model,
   what matters is the kind of bus a device is on: a NuBus card, a card in a processor-direct
   slot, a SCSI device, and so on.
2. **With the emulator and the frontend.** Here a device is seen through its *function*: what it
   provides, such as a hard disk, a CD-ROM drive, a floppy drive or a display. This part of the
   model has the structures and traits that present the device in the frontend, carry its
   configuration to it, report its status, and act on it, such as loading a disk image. It is
   independent of the bus: a NuBus card and a SCSI device can both provide Ethernet.
3. **The plumbing that links them together:** finding the devices of a machine, adding and
   removing them, making a machine from a list of devices, keeping them in workspaces and save
   states, and showing them in the debugger. These are the device operations (`DeviceOps`) and
   the bus operations (`BusOps`).

This document focuses mostly on the first and the third. The second is described briefly here, and
in more detail in [frontend.md](frontend.md).

## Intro to the model

The I/O devices of an emulated Mac that the frontend shows, configures or acts on (a NuBus card, a
floppy drive, a SCSI device, the video of the logic board, ...) are each represented in Snow as a
*device* in a *slot* on a *bus*. These are generic terms: a bus isn't always something a card or a
cable plugs into in the real machine. For example, `Built-in` is a bus that holds the devices of
the logic board, such as the video of a Macintosh LC. Each device provides one *function*, such as
HardDisk or Display, which is what the frontend deals with.

Some devices are chosen and configured by the user, such as a NuBus card. Others come with the Mac
model, such as its built-in video, and the user configures only part of them, or nothing.

What this model doesn't represent are the other chips of the logic board, such as the VIA or the
SCSI controller: they are created and managed by the Mac model itself, and aren't exposed to the
frontend or the user directly. The Mac model and its settings aren't devices either (see
[Terms](#terms)).

This document describes the device model: the structures in the core, how the core reaches a
device, how the frontend learns what the machine has, how a machine is made from a list of devices,
and how workspaces and save states keep them.

How a frontend uses the core is in [frontend.md](frontend.md). NuBus cards, their transactions and
their address spaces are in [nubus.md](nubus.md). The cards of the processor-direct slots are in
[pds.md](pds.md). The displays of a machine, one or several, are in [displays.md](displays.md).
Devices that read and write the machine's memory by themselves are in [dma.md](dma.md).

## Terms

- **Host:** the computer that runs Snow. **Guest:** the emulated Mac and the software it runs.
- **Core:** the `snow_core` crate, which emulates the machine. **Frontend:** the program that shows
  it and takes the user's input, such as `frontend_egui`.
- **Bus:** a list of slots. Some buses take devices while the machine runs (SCSI); others hold what
  the machine is built with (NuBus, the logic board's own devices).
- **Slot:** a place on a bus, which may hold a device.
- **Device model:** one kind of device, such as `ScsiHardDisk`. It fits one type of bus and
  provides one function.
- **Device:** one instance of a device model, in a slot. It owns all its state, and can have
  settings of its own, such as the file of a card's ROM.
- **Function:** what a device does for the user, such as HardDisk or Ethernet. It is a Rust trait
  whose methods act on the device, such as loading a disk image. The frontend has code for each
  function, and none for a device model.
- **Info, Config, Status:** three structs that a device model provides, in addition to the methods
  of its function. The **Info** has what the frontend needs to present the configuration options
  of the device to the user, such as the monitors of a video card. The **Config** carries the
  user's choices back to the device, such as the monitor that was chosen. The **Status** is the
  live state of the device, for the frontend to show, such as the title of the disk in a floppy
  drive. An action on a running device, such as loading a disk image, isn't in any of the three:
  it goes through the methods of the function. Each function defines the types of its three
  structs, and every device model that provides the function uses them. This generally spares the
  frontend any code specific to a device model: it only needs code specific to a function. A few
  special devices have a function of their own, such as the Apple IIe Card with AppleIIeCard.
- **Device operations:** what the emulator can ask of any device, whichever bus it is on: which
  device model it is, its function, and what the debugger shows of it. Every device implements
  them, as the trait `DeviceOps`.
- **Bus operations:** what the emulator can ask of a bus about its slots: which device is in a
  slot, put a device in a slot, remove it. Every bus implements them, as the trait `BusOps`.
- **Slot trait:** the trait of a bus type, with the methods its controller needs to talk to a
  device on that bus: SCSI commands for a SCSI target; reads, writes and an interrupt for a NuBus
  card. "Slot" is meant in the general sense of a place on a bus. It is independent of the
  function: a NuBus card and a SCSI target can both provide Ethernet.
- **Fixed:** said of a device model whose devices are made with the machine and never added or
  removed while it runs, such as floppy drives and NuBus cards.
- **Device config:** one device, as described in a list of devices: its bus, its slot, its device
  model, its ROM file and the Config of its function. It is the struct `DeviceConfig`. A workspace
  stores a list of device configs, and a machine is made from one.
- **Display:** a screen the frontend shows. Each display shows the frames of one video output
  device: a video card, or the machine's built-in video. Such a device is called a **video
  display**, and Display is its function. **Renderer:** a helper that each video display creates
  for itself. The video display draws each frame into it, and it passes the frame on to the
  frontend.
- **Mac model:** the machine, such as a Macintosh II (`MacModel`). This document never says "model"
  alone.
- **Variant:** Rust's word for one of the values of an enum. `ScsiHardDisk` is a variant of the
  enum `DeviceModel`. Unlike a value of a C enum, a variant can hold data: the variant `HardDisk`
  of the enum `FunctionConfig` holds a `HardDiskConfig`.

Settings of the machine itself aren't devices: the main ROM, the RAM size, PRAM, the CPU options
and the mouse mode are in `MachineConfig`. So is a board that the user only fits or not when the
machine is made, and that has nothing to set or show while it runs, as the FPU of an LC is: it is
a setting of the machine, not a device.

## An example machine

A Macintosh LC with the Apple IIe Card:

| Bus | Slot | Device model | Function |
| --- | --- | --- | --- |
| `SWIM` | `1` | `MacSuperDrive` | Floppy |
| `Floppy:PDS_LC.0` | `1` | `UniDisk35` | Floppy |
| `Floppy:PDS_LC.0` | `2` | `Apple525Drive` | Floppy |
| `SCSI` | `0` | `ScsiHardDisk` | HardDisk |
| `SCSI` | `3` | `ScsiCdrom` | Cdrom |
| `SCSI` | `4` | `DaynaPortScsiLink` | Ethernet |
| `Built-in` | `video` | `V8Video` | Display |
| `PDS_LC` | `0` | `AppleIIePdsCard` | AppleIIeCard |

The buses are one flat list. The Apple IIe Card adds a bus for its drives, `Floppy:PDS_LC.0`, to
that same list. It makes those drives itself, from its own Config, so they have no device config. A
device on the logic board is in a named slot of the `Built-in` bus: every machine's own video is at
`Built-in` / `video`. A controller with nothing to configure, such as the SCSI chip, isn't a device:
it only provides a bus.

What a serial port is connected to on the host isn't a device either at this point: a
pseudo-terminal, a TCP port or a LocalTalk network. The serial ports and their bridges are outside
the device model for now ([frontend.md](frontend.md#what-belongs-to-the-host)).

Several device models can provide the same function: a SCSI hard disk and a hard disk on another
bus both provide HardDisk. The frontend only deals with functions, so a new device model of an
existing function needs no frontend code.

The core tells the frontend what the machine has: the list of buses, their slots and the devices in
them, and the status of each device. The frontend acts on a device by sending the core a command,
such as "load this image in the drive in this slot".

## Adding a device model

This section is the checklist. The sections after it explain each piece.

### A device of a function that exists: a SCSI example

Say we add a removable-cartridge drive, a SyQuest, which the guest sees as a SCSI hard disk. It
provides the HardDisk function, which the frontend already handles, so only the core changes: a
new file for the device, and two lines in the list of device models.

**1. Name the device model.** Add a variant to `DeviceModel` in `core/src/devices/mod.rs`, and its
arm in `DeviceModel::info()`, which returns its `ModelInfo`:

```rust
pub enum DeviceModel {
    // ...
    ScsiSyQuest,
}

    Self::ScsiSyQuest => &mac::scsi::syquest::INFO,
```

The variant's name is what workspaces store: it must not change once released. A device model that
needs a cargo feature has `#[cfg(feature = "...")]` on both lines.

**2. Write the device**, in a new file, `core/src/mac/scsi/syquest.rs`, declared with
`mod syquest;` in `core/src/mac/scsi/mod.rs`. It implements three traits: the device operations,
`DeviceOps`; its bus's slot trait, `ScsiTarget`, which answers the SCSI commands; and the trait of
its function, `HardDiskFunction`:

```rust
#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetSyQuest {
    common: ScsiTargetCommon,
    // ... the device's own state
}

impl DeviceOps for ScsiTargetSyQuest {
    fn info(&self) -> &'static ModelInfo {
        &INFO
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::HardDisk(HardDiskConfig { media: /* its image file */, ..Default::default() })
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::HardDisk(self)
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetSyQuest {
    // ... inquiry(), read(), write() and the other SCSI commands
}

impl HardDiskFunction for ScsiTargetSyQuest {
    fn status(&self) -> HardDiskStatus { /* the image, its capacity */ }
    fn branch(&mut self, path: &Path) -> Result<()> { /* carry on from a copy */ }
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()> { /* its storage */ }
    fn savestate_image(&self) -> Option<&[u8]> { /* its image, for a save state */ }
}
```

`#[typetag::serde]` lets serde save and restore the device as a `Box<dyn ScsiTarget>`, with the
rest of the machine ([Save states](#save-states)).

**3. Write its create function**, which makes a device from a device config. It reads the Config of
the device's function from it, here a `HardDiskConfig` with the image file. A Config that names no
file makes a disk with no storage, which gets one later through `load_image()`.

```rust
fn create(config: &DeviceConfig) -> Result<MadeDevice> {
    let FunctionConfig::HardDisk(disk) = &config.function else {
        bail!("A SyQuest drive needs a hard disk's config");
    };
    Ok(MadeDevice::Scsi(Box::new(ScsiTargetSyQuest::new(disk.media.as_deref())?)))
}
```

**4. Write its `ModelInfo`**, which tells the frontend about the device model before any device of
it exists. Its `function` names the HardDisk function. That function has no Info struct, as a
hard disk has nothing for the frontend to ask:

```rust
pub const INFO: ModelInfo = ModelInfo {
    create: Some(create),
    ..ModelInfo::base(
        DeviceModel::ScsiSyQuest,
        "SyQuest drive",
        BusKind::Scsi,
        FunctionInfo::HardDisk,
    )
};
```

That is all. The Devices menu now offers the SyQuest drive on every empty SCSI slot, with the
same items as a hard disk ("Create new HDD image...", "Load HDD disk image..."). Workspaces save
and load it, and save states keep it. A test can check the round trip, as
`the_machine_is_reported_as_the_list_it_is_made_from` in `core/src/emulator/devices.rs` does:
make a machine with the device, report it with `devices()`, make a second machine from the
report, and compare.

### Other cases

- **A NuBus card** implements `DeviceOps` too, and `NubusSlot` instead of `ScsiTarget`. Like a SCSI
  target, it is complete in its own file: no list of cards has to name it. Its create function
  returns `MadeDevice::Nubus`. It is `fixed`: the Load ROM dialog offers it in the NuBus slots, from
  its `ModelInfo`. Its ROM file is the `rom` of its device config. A card with video provides the
  Display function: the monitor on its connector is in the `DisplayConfig` of its device config, and
  it creates its own renderer ([displays.md](displays.md)). See [nubus.md](nubus.md).
- **A built-in device**, on the logic board, is a field of the machine's bus. It can provide any
  function: a video, or an Ethernet controller. It implements `DeviceOps` and no slot trait, and
  has no create function: the machine's constructor makes it, and reads the Config of its function
  from its device config. Its `fits` is `MacModel::has_built_in()`. It goes in a named slot of the
  `Built-in` bus, which its machine states in two places: the machine's table of built-in
  devices, which `MacModel::built_in()` returns for the Mac model and from which
  `MacModel::buses()` makes the slots of the `Built-in` bus, and the `match` of the machine's
  `BusOps::device()`
  ([How the emulator reaches a device](#how-the-emulator-reaches-a-device-busops-and-deviceops)).
  The slot id is a constant in `built_in`.
- **A device that its machine always has, on a bus other than `Built-in`**, such as a floppy
  drive, has a create function like any device in a slot. The machine names it as the device of
  that slot in `MacModel::buses()`, and `Emulator::new()` makes it and attaches it, as it does the
  devices of the list ([How a machine is created](#how-a-machine-is-created)).
- **A new function** needs frontend code to show it and ask for what it needs. In the core: its
  trait, and its Info, Config and Status structs next to each other, in
  `core/src/devices/function.rs`; and a variant in `FunctionInfo`, `FunctionConfig`, `FunctionMut`
  and `FunctionStatus`. In the frontend: its items in the Devices menu, and what the Load ROM
  dialog asks for it.
- **A new kind of bus** is a change to the core's model, not only an addition: it needs a
  `BusKind`, a bus id in `bus_id`, its buses in `MacModel::buses()` with the device of each slot
  that the machine has without being asked, a variant of `MadeDevice` for its devices, and a
  `BusOps` implementation on the object in which the machine holds the devices of that bus,
  returned by the machine's `bus_ops()` for that bus id
  ([How the emulator reaches a device](#how-the-emulator-reaches-a-device-busops-and-deviceops)).
  `BusOps::attach()` and `detach()` refuse by default: a bus whose devices are made by create
  functions must implement `attach()`, or every device config for it is refused. If its slots are
  numbered, the conversion from a slot id to a slot number is private to its own file
  ([Buses and slots](#buses-and-slots)). A slot trait is only needed when the bus can hold devices
  of different types ([Making a device](#making-a-device)).
- **A new machine** needs, in the core: a variant of `MacModel` with its ROM digests in
  `MacModel::ROMS` and its arm in each `match` on `MacModel` that has no wildcard; a variant of
  `EmulatorConfig`, with its arm in each block of the `dispatch!` table and in `Emulator::new()`;
  and a bus struct with what that table calls (`bus_ops()`, `persist_pram()`, `set_datetime()`,
  the input methods and others) and the traits `Bus`, `Tickable`, `IrqSource`, `InspectableBus`
  and `Debuggable`. Its constructor has the same two arguments as the others and returns a
  `Result`. Its built-in devices are in its table, as above. The compiler lists most of it.

## Structures

The types of the device model are in `core/src/devices/`. Each device model's `ModelInfo` and
trait implementations are in that device's own file.

### Buses and slots

```rust
pub enum BusKind { Scsi, Swim, Nubus, LcPds, Se30Pds, IieDisk, BuiltIn }

pub struct Bus {
    pub kind: BusKind,
    pub id: String,            // "SCSI", "Floppy:PDS_LC.0". In workspace files: must not change.
    pub title: String,         // In the menus: free to change
    pub slots: Vec<Slot>,
}

pub struct Slot {
    /// The slot's id: unique on its bus, and the only way to identify the slot outside its
    /// bus. It may read like a number ("3", "D"), but nothing outside the bus treats it as
    /// one. In the workspace files: must not change.
    pub id: String,            // "3", "9", "video"
    pub device: Option<Device>,
}

pub struct Device {
    pub model: DeviceModel,
    pub rom: Option<PathBuf>,      // Its ROM file, if it has one
    pub function: FunctionConfig,  // The Config of its function, as it is now
    pub name: Option<String>,      // In the menus, if the device gives one
}
```

The struct `Device` reports the device model, the ROM path and the Config of the device in a slot,
as they are now, so that they can be read easily when a workspace is saved
([Workspaces](#workspaces)).

`Bus`, `Slot` and `Device` describe the machine. They are plain data, which the frontend gets in
the `Buses` event. The buses and the devices themselves, inside the machine, are reached through
the `BusOps` and `DeviceOps` traits
([How the emulator reaches a device](#how-the-emulator-reaches-a-device-busops-and-deviceops)).

**A slot id is not a number.** Outside its bus, a slot is known by its id and by nothing else: in
the emulator's device code, in commands, in workspaces, in save states and in the frontend. Some
ids read like numbers, `"3"` on the SCSI bus or `"D"` on the NuBus, but they are strings all the
same. Code outside the bus compares them, shows them and passes them back, and never computes with
them.

**A slot number belongs to its bus.** Inside, a bus may know its slots by a number: the NuBus slot
`$9` to `$E`, the place of a drive. The id and the slot number don't have to match: the floppy
drive with the id `"1"` is drive 0 inside its bus. That slot number never leaves the bus. The
conversion from a slot id to a slot number is private to the bus's own file, so no other code can
get a slot number. `MacModel::buses()` has no numbers either: the slots of each bus of a Mac model
are listed by id. The `Built-in` bus has no slot numbers, only ids.

**SCSI is the one exception.** A SCSI ID is well defined and known to the user, so the core and
the frontend may both convert between a SCSI ID and the slot id, which is the ID written in
decimal.

A bus, too, is known everywhere by its id, such as `"SCSI"`.

| Bus | Slot ids | Slot number, in the bus's own code |
| --- | --- | --- |
| `SWIM` | `1` to `3` | The drive in `Swim::drives`, 0 to 2 |
| `SCSI` | `0` to `6` | The SCSI ID |
| `NuBus` | `9` to `E` | The slot, `$9` to `$E` |
| `PDS_LC` | `0` | 0 |
| `PDS_SE30` | `0` | None: nothing goes in it yet |
| `Floppy:PDS_LC.0` | `1`, `2` | The place on the Apple IIe Card's chain, 0 or 1 |
| `Built-in` | `video` | None: the machine's `BusOps::device()` names each slot |

A test checks, for every Mac model, that its bus accepts each slot id `MacModel::buses()` lists.

The list has no hierarchy, and a bus has no parent. A bus that a device adds says so in its id:
the drives of the Apple IIe Card in the LC PDS slot are `Floppy:PDS_LC.0`.

`MacModel::buses()` returns the list of buses a Mac model has before any optional device is added,
with the default device of each slot. It is a description only: no device is created, and no
machine needs to exist, so the Load ROM dialog and the workspace code call it first. The default
device of a slot is the one the machine gets there when no device config names that slot, such as
a floppy drive or the built-in video. It is given as a `Device` struct: its device model, no ROM
file, and a Config in which nothing was chosen.

| Bus id | On | Slots |
| --- | --- | --- |
| `SWIM` | Every machine with a SWIM or an IWM, `MacModel::has_swim()` | `1` upwards, one per drive |
| `SCSI` | Every Mac with SCSI | `0` to `6` |
| `Built-in` | Every Mac with a built-in video | `video` |
| `NuBus` | The Mac II family and the Quadras | Their card slots, `9` to `E` |
| `PDS_LC` | The LC family | `0` |
| `PDS_SE30` | The SE/30 | `0` |

PDS slots come in incompatible types, so each type is a `BusKind` of its own, with its own bus id
and device models. The Apple IIe Card fits only `LcPds`, so it can't be put in an SE/30.

### Device models and their `ModelInfo`

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

A workspace file stores a device model as text, by the name of its variant:
`"model": "ScsiHardDisk"`. So a variant of this enum must not be renamed once released.

A device model is one piece of hardware. The built-in colour videos are five device models, one
per chip, as each chip has its own monitors and VRAM sizes:

| Device model | Chip | On |
| --- | --- | --- |
| `V8Video` | V8 | LC, LC II |
| `SonoraVideo` | Sonora | LC III |
| `DafbVideo` | DAFB | Quadra 700 |
| `MemcJrVideo` | MEMCjr | LC 475 |
| `DjMemcVideo` | djMEMC | Quadra 800 |

Each device model has one static `ModelInfo`, in its own source file. The frontend reads it to
learn about a device model before any device of it exists:

```rust
pub struct ModelInfo {
    pub model: DeviceModel,
    pub name: &'static str,                       // In the menus: free to change
    pub bus: BusKind,                             // The bus type it fits
    pub fits: fn(MacModel) -> bool,               // Which Mac models with that bus it fits
    pub fixed: bool,                              // Never added or removed once the machine is made
    pub rom: Option<RomInfo>,                     // The ROM it needs, if any
    pub function: FunctionInfo,                   // Its function, and what that function supports
    /// How a device of this device model is made. None: it is on the logic board.
    pub(crate) create: Option<fn(&DeviceConfig) -> Result<MadeDevice>>,
}

pub struct RomInfo {
    pub label: &'static str,               // In the dialog: "Display Card 8-24 ROM"
    pub digests: &'static [&'static str],  // SHA-256 of each ROM known to work, in hex
    pub optional: bool,                    // The device works without the file
}
```

**`RomInfo` describes the ROM file a device model needs.** Some devices have a ROM of their own,
apart from the Mac's ROM, such as the declaration ROM of a video card. Snow doesn't come with
these ROMs: the user provides the file, and its path is the `rom` of the device's device config.
`label` is how the Load ROM dialog names the file when it asks for it. `digests` has the SHA-256
digest of each file known to work, so the dialog can tell the user that a file isn't one of
them. `optional` is for a device that works without a file: the core has contents for its ROM,
and a file the user provides replaces them. When `optional` is false, a device config with no
`rom` is refused. A device model with no ROM of its own has `rom: None`.

The fields are of two sorts:

- **The same for every device model, whatever it does:** `name`, `bus`, `fits`, `rom` and `fixed`.
  They tell the frontend where and whether to offer the device model.
- **`function`, which depends on what the device does.** It is an enum with one variant per
  function. A variant holds the Info of that function: what a device model supports, which the
  frontend offers the user. For a video display, `FunctionInfo::Display(DisplayInfo)` lists the
  monitors and the VRAM sizes it can have. The frontend looks at nothing else to know what to show
  for a device.

```rust
pub enum FunctionInfo {
    Floppy(FloppyInfo),
    HardDisk,
    Cdrom,
    Ethernet(EthernetInfo),
    Printer,
    Display(DisplayInfo),
    AppleIIeCard,   // The frontend has code written for this card: nothing says what to ask yet
}
```

The Info, Config and Status structs of each function, with what each holds, are described in
[frontend.md](frontend.md#functions-what-to-ask-the-answers-and-the-status), since a frontend is
what uses them.

Most `ModelInfo` are a few lines: `ModelInfo::base()` fills in what most device models have, and
each `ModelInfo` sets the rest:

```rust
pub const INFO: ModelInfo = ModelInfo {
    create: Some(create),
    ..ModelInfo::base(
        DeviceModel::ScsiHardDisk,
        "Hard disk",
        BusKind::Scsi,
        FunctionInfo::HardDisk,
    )
};
```

`DeviceModel::info()` is a `match` with one arm per variant, returning that device model's
`ModelInfo`, so the compiler refuses a device model with none. `models_for(bus, mac)` is what a
slot on this kind of bus can take on this Mac model: the frontend's menus and dialogs list it, and
the core refuses anything else.

**Built-in devices have a `ModelInfo` too.** It has `BusKind::BuiltIn` as its `bus`, and no
`create`: every other device model has a create function. Which slot of the `Built-in` bus a
built-in device is in isn't in its `ModelInfo`: the slot belongs to the machine. Each machine has
a table of its built-in devices, with the slot id and the device model of each:

```rust
impl LcIBus {
    pub const BUILT_IN: &[(&str, DeviceModel)] = &[(built_in::VIDEO, DeviceModel::V8Video)];
}
```

`MacModel::buses()` makes the slots of the `Built-in` bus from that table, each with the device
model of the table and the Config of its function in which nothing was chosen,
`FunctionInfo::default_config()`. The frontend learns from it that a Macintosh LC has a built-in
video, and which monitors and VRAM sizes to offer for it, the same way as for a video card. A
built-in device of another function, such as an Ethernet controller, is listed the same way.

Whether a device model is `fixed` decides what the menus offer and what can be removed:

| Where | What a slot can take | What can be removed |
| --- | --- | --- |
| Devices menu, while the machine runs | Device models that aren't `fixed`, in empty slots | Devices that aren't `fixed` |
| Load ROM dialog | `fixed` device models | Not applicable: it builds a new machine |

### Making a device

Every device model that goes in a slot has a create function: `create`, a function pointer in its
`ModelInfo`. The emulator calls it with a device config, and it returns the new device.

A type of bus whose devices can be of different types holds them through its own slot trait: the
SCSI bus holds `ScsiTarget` objects, the NuBus holds `NubusSlot` objects. A bus whose controller
only ever holds one type of device may hold that type directly, with no slot trait. But `create`
has the same type for every device model, so it has one return type, `MadeDevice`: a new device,
tagged with the type of bus it goes on. In C, it would be a tagged union of pointers, one per type
of bus.

```rust
pub(crate) enum MadeDevice {
    Scsi(Box<dyn ScsiTarget>),     // A device for a SCSI bus
    Swim(Box<dyn SwimBusDevice>),  // A floppy drive
    Nubus(Box<dyn NubusSlot>),     // A NuBus card
    Pds(Box<dyn PdsSlot>),         // A card for a processor-direct slot
}
```

The emulator gives the `MadeDevice` to the bus that the device config names, with
`BusOps::attach()`. The bus checks that it got the variant of its own type, and puts the device in
the slot.

A device model whose `ModelInfo` has `fixed` set, such as a card or a floppy drive, is made this
way too, but only by `Emulator::new()`, while it makes the machine: it is refused once the machine
is made.

Only the devices on the logic board have no create function, such as a built-in video. They are
part of their machine, and its constructor makes them. It gets their device configs, as
[How a machine is created](#how-a-machine-is-created) says.

The create function of the Apple IIe Card also makes the card's drives: its `IieCardSettings`
says which drive models are on its disk port. It makes each with the drive's own create function,
and attaches it to the card's bus.

A create function has the device config as its only argument. What the device needs from the host
is named there, and the device opens it by that name: a hard disk opens its image file, a
DaynaPORT its network link, and a printer creates its files in its folder.

Two things of the host can't be named in a device config, because they are objects of the running
program and not values that can be written to a file. A device gets them after it is made, through
a method of its function:

- **The sound output of the frontend**, on which a CD-ROM drive plays audio CDs:
  `CdromFunction::set_audio_provider()`. The emulator calls it when the frontend sets the sound
  output, and when a drive is attached after that. The sound output isn't in a save state: the
  frontend sets it again after a state is loaded, as for a new machine.
- **A disk image that isn't a file**, of a frontend that keeps its images itself, as Infinite Mac
  does: `HardDiskFunction::load_image()` and `CdromFunction::load_image()`. A hard disk whose
  Config names no file is made with no storage, and answers Not Ready to the guest until it is
  given one.

The default folder for printed pages reaches a printer the same way, through
`PrinterFunction::set_default_dir()`. The emulator calls it on every printer when the frontend
gives the folder, and on a printer that is added after that. A printer uses it when its Config
names no folder. It keeps it apart from its Config, and it isn't in a save state
([frontend.md](frontend.md#what-belongs-to-the-host)).

A device with a ROM reads the file its device config names in `rom` when it is made. The Load ROM
dialog checks the file against the digests in the device model's `ModelInfo` before any device
exists, unless the user turns ROM validation off.

### Functions and their status

A function is the interface from the frontend to a device: a trait whose methods act on the
device. Its name ends in `Function`, which tells it apart from the slot trait the same device
implements for its bus. The function holds no data. Each function comes with a Status: a struct
that says how the function stands, such as a floppy drive's disk title, or whether its disk has
unsaved writes.

```rust
pub trait FloppyFunction {
    fn status(&self) -> FloppyStatus;
    fn load(&mut self, path: &Path, read_only: bool) -> Result<()>;
    fn eject(&mut self);

    // What only some drives can do, which the FloppyInfo of their ModelInfo says.
    // Refused by default.
    fn insert_image(&mut self, image: Box<FloppyImage>, read_only: bool) -> Result<()>;
    fn save(&mut self, path: &Path) -> Result<()>;
    fn set_writeback(&mut self, enabled: bool) -> Result<()>;
    fn set_rpm_adjustment(&mut self, rpm: i32) -> Result<()>;

    // Called by the emulator on every floppy drive. They do nothing by default.
    // Writes the disk back to the file it came from, if write-back is on and a save is due
    fn write_back(&mut self, force: bool) -> Result<()>;
    // Takes the image of the last disk ejected, which the frontend can put back or save
    fn take_ejected_image(&mut self) -> Option<Box<FloppyImage>>;
    // Seeds the random bits of the drive, for runs that must repeat. None: random again.
    fn seed_weak_bits(&mut self, seed: Option<u64>);
}

pub trait HardDiskFunction {
    fn status(&self) -> HardDiskStatus;
    fn branch(&mut self, path: &Path) -> Result<()>;  // Carries on from a copy of the image
    // The disk's storage, for a frontend that keeps its images itself, and after a save state
    // is loaded. Refused if it has one.
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()>;
    // The disk's image, for a save state. Called by the emulator when it saves one.
    fn savestate_image(&self) -> Option<&[u8]>;
    // Replaces the disk's image with the file `path`, on a drive that can't be removed while
    // the machine runs, such as an HD20. Refused by default.
    fn load(&mut self, path: &Path) -> Result<()>;
}

pub trait CdromFunction {
    fn status(&self) -> CdromStatus;
    fn load(&mut self, path: &Path) -> Result<()>;
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()>;
    fn set_folder(&mut self, folder: &Path) -> Result<()>;  // A folder of images the guest swaps
    // Called by the emulator to give the drive the host's sound output, for audio CDs
    fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()>;
}

pub trait EthernetFunction {
    fn status(&self) -> EthernetStatus;
    fn set_link(&mut self, link: EthernetLinkType) -> Result<()>;
    // Packet capture to a pcap file. Legacy interfaces: will be removed when Ethernet support
    // is split out of the DaynaPORT.
    fn start_capture(&mut self, filename: &Path) -> Result<()>;
    fn stop_capture(&mut self) -> Option<(PathBuf, usize)>;
}

pub trait PrinterFunction {
    fn status(&self) -> PrinterStatus;
    // The folder for its pages when its Config names none. Called by the emulator.
    fn set_default_dir(&mut self, dir: &Path);
}

pub trait DisplayFunction {
    fn status(&self) -> DisplayStatus;
    fn frames(&self) -> FrameReceiver;   // Where the frontend picks up this display's frames
    fn blank(&mut self) -> Result<()>;   // Shows a blank screen, when the machine resets or stops
}

// What only the Apple IIe Card has
pub trait AppleIIeCardFunction {
    fn in_front(&self) -> bool;                   // As the event `IieInFront` reports it
    fn set_game_port(&mut self, port: GamePort);  // The state of the joystick, from the frontend
}
```

A video display provides the Display function. It creates its own renderer, draws each frame into
it, and gives the frontend the other end with `frames()`. Nothing is handed to a video display for
this, when it is made or when a save state is loaded, and the displays of a machine are simply the
devices of its bus list whose function is Display. [displays.md](displays.md) has the details.

The emulator only sees a device through its function:

```rust
pub enum FunctionMut<'a> {
    Floppy(&'a mut dyn FloppyFunction),
    HardDisk(&'a mut dyn HardDiskFunction),
    Cdrom(&'a mut dyn CdromFunction),
    Ethernet(&'a mut dyn EthernetFunction),
    Printer(&'a mut dyn PrinterFunction),
    Display(&'a mut dyn DisplayFunction),
    AppleIIeCard(&'a mut dyn AppleIIeCardFunction),
}
```

`FunctionInfo`, `FunctionConfig` and `FunctionStatus` have the same variants, in the same order.
`FunctionMut::status()` returns the `FunctionStatus` of the device, whichever its function is: the
emulator calls it for each device when it sends the statuses.

A Mac drive and an Apple IIe Card drive implement the same `FloppyFunction`, so the menu has one
code path and names no device model. What a drive takes and can do beyond loading and ejecting, such
as which blank disks it takes and whether it ever writes to its image (`writable`), is in the
`FloppyInfo` of its `ModelInfo`, and what it can't do, it refuses.

**The emulator calls two methods of `FloppyFunction` by itself**, and not for a command of the
frontend:

- **`write_back()` saves a disk to the file it came from.** A drive only does so when the user
  turned write-back on for that disk, with `set_writeback()`. The emulator calls `write_back()` on
  every floppy drive after each batch of instructions and at each status update: the drive then
  saves if its motor stopped since the last write. It calls it with `force` before the frontend
  ejects a disk and when it quits: the drive then saves even while the disk turns. The emulator
  shows a failed save to the user.
- **`take_ejected_image()` gives the image of a disk that was ejected.** The emulator calls it at
  each status update, and sends what it gets to the frontend in the `FloppyEjected` event. A drive
  gives an image once.

**Status isn't state.** A device owns its state, such as a floppy drive's head position, and
`status()` only reads it. Save states store the state with serde, the serialization library.

**Info, Config and Status go together.** Each function has up to three structs, defined next to
each other in `function.rs`:

| Struct | What it is | Where its value is found |
| --- | --- | --- |
| Info, such as `DisplayInfo` | What the frontend needs to present the configuration options of a device. A constant of the device model. | In the `ModelInfo` of the device model |
| Config, such as `DisplayConfig` | The user's choices for one device | In the `DeviceConfig` of the device |
| Status, such as `DisplayStatus` | The live state of one device | Returned by `status()`, a method of the function. The frontend gets it in the `DeviceStatus` event. |

What each holds for each function is in
[frontend.md](frontend.md#functions-what-to-ask-the-answers-and-the-status). A device model has
no settings struct of its own: what is chosen for a device is the Config of its function, and its
ROM file, which is a field of `DeviceConfig` because any device model can need one
([`DeviceConfig`](frontend.md#deviceconfig-one-device)).

A floppy drive has no Config: a workspace doesn't keep the disk in a drive, nor the disc in a
CD-ROM drive.

### Device operations and slot traits

Every device implements the `DeviceOps` *trait*, whichever bus it is on. It is what the emulator's
device code asks of a device:

```rust
pub(crate) trait DeviceOps {
    fn info(&self) -> &'static ModelInfo;                // Which device model it is
    fn function(&mut self) -> FunctionMut<'_>;           // The device, seen through its function
    fn rom(&self) -> Option<&Path>;                      // Its ROM file. None by default.
    fn config(&self) -> FunctionConfig;                  // The Config of its function, as it is now
    fn name(&self) -> Option<String>;                    // Its name for the user. None by default.
    fn debug_properties(&self) -> DebuggableProperties;  // For the debugger. Nothing by default.
    // The buses this device adds to the machine. None by default.
    fn extra_buses(&mut self) -> Vec<(Bus, &mut dyn BusOps)>;
}
```

`config()` returns the Config the device was made with, with the changes the user asked for since:
a hard disk that was branched to another image file names that file. A device doesn't have to keep
the struct it was given: it only has to return the right one. What the core worked out by itself
isn't in it. A printer made with no `output_dir` returns none, whichever folder its pages go to,
and a video made with no monitor chosen returns none, though it shows the default monitor. Those
are in the device's Status.

`name()` returns what the menus call this device: "LaserWriter IISC", "Apple IIe UniDisk 3.5". A
device can use what the `connected()` of its slot trait told it, described below, such as its
connector. The method is optional. For a device that returns none, the frontend writes the text
itself, from the bus's title, the slot's id and the device's function: "Floppy #1". The SCSI menu
is a special case: the frontend always writes "SCSI #5" itself, as a SCSI target isn't told its ID,
and shows the device's name after it.

Additionally, each bus type has a trait of its own, with the methods its controller needs to talk
to a device on that bus. It is a subtrait of `DeviceOps`: a device that implements it must
implement `DeviceOps` too. For example, the SCSI bus has `ScsiTarget`. The bus hands each SCSI
command to the device through `cmd()`, as its command descriptor block:

```rust
#[typetag::serde(tag = "type")]
pub(crate) trait ScsiTarget: Send + Debuggable + DeviceOps {
    fn cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult>;
    // ...
}
```

`cmd()` has a default body, which decodes the commands that every target has in common. For what
differs from one device to the next, it calls other methods of the trait, such as `inquiry()` and
`read()`, which each device supplies.

By convention, this document calls that trait the bus's **slot trait**, with "slot" in the general
sense of a place on a bus: a NuBus slot, but also a SCSI ID.

The guest talks to a device through the controller of its bus, and the slot trait is that
conversation. The function is separate: it is how the frontend acts on the device.

| Bus type | Slot trait | Its methods |
| --- | --- | --- |
| SCSI | `ScsiTarget` | Each SCSI command, through `cmd()` |
| NuBus | `NubusSlot` | Transactions in its slot spaces, and an interrupt |
| Processor-direct slot | `PdsSlot` | Cycles of the CPU in the slot's address ranges, its interrupts, and the sound line |
| SWIM, and the disk port of the Apple IIe Card | `SwimBusDevice` | The lines of a drive connector, and whole bytes for a device that uses them |

The `Built-in` bus is the exception. It has no slot trait, because there is no controller between
the machine and its built-in devices: the machine's code uses them directly, as its own fields.

The controller of a bus talks to the devices of that bus through the slot trait. The emulator's
device code, which doesn't need the specifics, talks to them through the common `DeviceOps`.

**For performance reasons, the `SWIM` bus does things a little differently.** Its slot trait,
`SwimBusDevice`, is the drive connector: it has the lines of the connector, and whole bytes for a
device that exchanges bytes with the controller in place of the bits of a disk. Today that is the
UniDisk 3.5 of the Apple IIe Card; the HD20 hard disk will be another. But the bits of a floppy
disk don't go through the trait. The controller reads and writes them one by one for as long as a
drive turns, and a call through a trait for each bit would be too slow. So a floppy drive gives
the controller a struct, `FloppyStream`, and the controller reads and writes the bits on it with
direct calls.

```rust
/// What the floppy controller sees of a device on a drive connector.
///
/// The methods here are for what happens rarely: the lines of the connector, and whole bytes
/// for a device such as a UniDisk 3.5. The bits of a floppy disk do not go through this trait: a
/// floppy drive hands out its `FloppyStream` with `stream()`, once per controller tick, and
/// the controller reads and writes the bits on it directly. See `FloppyStream` for why.
pub(crate) trait SwimBusDevice: DeviceOps + Send {
    /// Called by the machine to specify the connector and the tick rate, once, when the bus
    /// attaches the device and before any other method.
    ///
    /// `connector` is the place of the device on its bus, from 0. `tick_rate` is the number of
    /// ticks in one second, for the ticks that `tick()` counts: the device needs it to turn a
    /// duration into ticks, such as the time its head needs to settle, or one turn of its disk.
    /// Neither is in the device's config: nobody chooses them, and only the machine knows them.
    fn connected(&mut self, connector: usize, tick_rate: Ticks);

    // The lines of the connector
    fn set_lines(&mut self, lines: u8);        // The control lines of the connector changed
    fn strobe(&mut self, lines: u8) -> bool;   // The strobe line rose. True: see below.
    fn sense(&self, lines: u8) -> bool;        // The level of the sense line
    fn set_pwm(&mut self, pulse: u8);          // One pulse of the PWM line. Nothing by default.
    fn tick(&mut self, ticks: Ticks);          // Time passing, whether selected or not
    fn reset(&mut self);                       // The machine resets. Nothing by default.

    // For a floppy drive: its disk, spinning or not. None by default.
    fn stream(&mut self) -> Option<&mut FloppyStream>;

    // For a device that exchanges whole bytes: a UniDisk 3.5, later an HD20. Nothing by default.
    fn read_byte(&mut self, ticks: Ticks) -> Option<u8>;
    fn write_byte(&mut self, byte: u8);
}

/// The disk under the heads of a floppy drive, which the controller reads and writes bit by bit.
///
/// This is a struct with ordinary methods, and not part of the `SwimBusDevice` trait, on
/// purpose. The controller talks to its devices through a trait, so a trait method is a call
/// through a table of function pointers. That is fine for the lines of the connector, which
/// change when the guest accesses a register. It is too slow for the bits of a disk, which the
/// controller reads one by one for as long as the drive turns. A method of this struct is a
/// direct call, which the compiler can inline.
pub struct FloppyStream {
    pub(super) spinning: bool,  // The disk turns: the controller reads and writes its bits
    /* The disk image, the track, the position on it */
}

impl FloppyStream {
    pub fn ticks_per_bit(&self) -> Ticks { /* ... */ }
    pub fn next_bit(&mut self, head: usize) -> bool { /* ... */ }
    pub fn write_bit(&mut self, head: usize, bit: bool) { /* ... */ }
}
```

The controller talks to its devices through the `SwimBusDevice` trait and doesn't know their
types: what a floppy drive or a UniDisk 3.5 is made of is private to that device. The controller
asks the selected device for its stream.

- **A floppy drive always gives its stream**, and the stream says whether the disk is spinning,
  in `FloppyStream::spinning`. The controller runs its bit loop on a stream that spins. It does
  nothing with one that doesn't: a byte the guest wrote stays in the controller.
- **A device with no stream exchanges bytes with the controller instead**, with `read_byte()` and
  `write_byte()`: a UniDisk 3.5 on the disk port of the Apple IIe Card, an HD20 on a Mac. The
  Mac's controller exchanges bytes in IWM mode.

**The PWM line.** A Macintosh 128K, 512K or Plus sets the speed of its drives with a PWM signal on
the drive connector. The controller gives each pulse to every device, with `set_pwm()`. A drive
whose speed doesn't follow the signal leaves the method empty.

**Chained devices.** Devices can be chained on one connector, as HD20s are. `strobe()` returns
true when the device passes the selection on to the next device of the chain. The Mac's controller
keeps a chain of up to four devices in front of the drive of its external connector: the selection
moves along it at each strobe that a device passes on, and goes back to its start when the
connector is no longer enabled. Past the last device of the chain, the drive of the connector
answers.

**A reset.** When the machine resets, the controller calls `reset()` on each of its devices, as
the machine does on a NuBus card or a PDS card. A device that was in the middle of an exchange
drops it.

**Note: the controller doesn't call a device on each of its ticks.** This is an optimisation, of
the same sort as the CPU giving the bus its ticks in batches. Calling `tick()` and `stream()`
through the trait on every tick cost 3 to 5% of the speed of a Macintosh II whose drives were
idle. So:

- The controller adds up the ticks that pass, and gives the total to every device, with `tick()`,
  before it calls any other method of a device. A device has the controller's time whenever it
  is called.
- When the stream of the selected drive doesn't spin, the controller doesn't ask for it again
  until the controller or the emulator next calls that device.
- This asks one thing of a device: its state only changes inside one of its methods. What
  depends on time, such as a motor that stops after a delay, is worked out from the ticks it is
  given.
- The emulator reaches a drive through the controller, which gives it its time first. That is
  why `Swim`, the controller, implements `BusOps` for the `SWIM` bus, and not its array of
  connectors.

**A device learns where it is from `connected()`.** A create function has only the device config,
which says nothing of the machine. So when the bus attaches a device, it calls `connected()` with
the place of the device's connector and the rate of the controller's ticks. A floppy drive counts
time in those ticks.

This is a convention of the slot traits, not of this one alone. Each has a `connected()`, with
what a device can know of its place on that type of bus:

| Slot trait | `connected()` gives |
| --- | --- |
| `SwimBusDevice` | The connector, and the tick rate |
| `NubusSlot` | The slot number, `$9` to `$E`, and the tick rate |
| `PdsSlot` | The tick rate: a machine has one slot |

It is in each slot trait, and not once in `DeviceOps`, because a slot number belongs to its bus
and never leaves it. `ScsiTarget` has none.

**The four Mac floppy drives are one Rust type.** `Mac400KDrive`, `Mac800KDrive`,
`Mac800KPwmDrive` and `MacSuperDrive` are four device models, each with its `ModelInfo` and its
create function, and each create function makes a `FloppyDrive` of that kind.

**The disk port of the Apple IIe Card uses the same slot trait.** The card's disk controller is an
IWM, as the floppy controller of the early Macs is, and the devices on its port are of the same
two sorts. A 5.25" drive is a floppy drive: it gives the controller a `FloppyStream`. A UniDisk
3.5 exchanges whole bytes with the controller, as an HD20 does. The control lines that
`set_lines()` gets are CA0, CA1, CA2 and SEL on a Mac, and the four phase lines of the disk port
on the card. The two buses are of different types all the same, `BusKind::Swim` and
`BusKind::IieDisk`, so a Mac drive can't go on the card, nor a 5.25" drive on a Mac.

## How the emulator reaches a device: `BusOps` and `DeviceOps`

A device is reached two ways.

The guest reaches it through the slot trait: a CPU access goes to the controller of the bus, which
talks to the device through the slot trait. A built-in device has no controller, and the machine's
code uses it directly.

The emulator's device code runs the frontend's commands. What it needs of a device is mostly its
function: the trait through which the frontend acts on the device, such as `FloppyFunction` or
`HardDiskFunction` (see [Functions and their status](#functions-and-their-status)). Every device
gives its function through `DeviceOps::function()`.

Some operations don't depend on the function, and some have no device to ask yet: finding the
device in a slot, putting a device in an empty slot, and removing it. Those are asked of the bus,
through `BusOps`, a trait each bus implements.

For example, to load an image in the floppy drive in slot `1` of the `SWIM` bus, the emulator's
device code goes through both:

1. `machine.bus_ops("SWIM")` → the bus, as a `BusOps`
2. `bus.device("1")` → the drive in slot `1`, as a `DeviceOps`
3. `device.function()` → the drive's function, a `FloppyFunction`
4. `floppy.load(path, read_only)` → the image is loaded

The rest of this section is about `BusOps`.

For the SCSI bus of a Mac II:

```
 guest's side                              emulator's device code
 (each CPU access, each tick)              (commands, statuses, debugger)
        |                                          |
        | the SCSI controller                      | bus_ops("SCSI"),
        |                                          | as a &mut dyn BusOps
        v                                          v
        +----------------->  ScsiBus  <------------+
                                |                  |
          the slot trait,       |                  | device("0"),
          ScsiTarget::cmd()     v                  v as a &mut dyn DeviceOps
                    targets[0]: the hard disk at ID 0
```

```rust
pub(crate) trait BusOps {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps>;      // The device in a slot
    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()>;  // Refused by default
    fn detach(&mut self, slot: &str) -> Result<()>;                      // Refused by default
}
```

`slot` is the slot's id, a string that names the slot: `"3"` on the SCSI bus, `"D"` on the NuBus,
`"video"` on the `Built-in` bus. Inside, a bus may turn it into a slot
number, which may or may not be the id read as a number. That is entirely private to the bus's
implementation. For the SCSI bus, `device()` is:

```rust
fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
    let target = self.targets[slot_number(slot)?].as_mut()?;
    Some(&mut **target)    // A SCSI target is a DeviceOps too
}
```

The objects that implement `BusOps` are the ones the machine already has:

| Bus | Implemented by |
| --- | --- |
| `SCSI` | `ScsiBus` |
| `SWIM` | `Swim`, the floppy controller, with its drive connectors |
| `NuBus` | `Nubus` |
| `PDS_LC`, `PDS_SE30` | `Pds`, the slot of the card ([pds.md](pds.md)) |
| `Floppy:PDS_LC.0` | `DiskII`, the card's disk controller, with its drive connectors |
| `Built-in` | Each machine's bus, whose built-in devices are its own fields |

The `Built-in` bus has no array of slots, and no object of its own: a built-in device is a field
of its machine, which uses it directly. So the machine is the `Built-in` bus, and its
`BusOps::device()` gives the field for a slot id. For the compact Macs, whose only built-in device
is the video:

```rust
impl BusOps for CompactMacBus {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        match slot {
            built_in::VIDEO => Some(&mut self.video),
            _ => None,
        }
    }
}
```

A machine names each built-in slot twice: in this `match`, and in its table of built-in devices
([Device models and their `ModelInfo`](#device-models-and-their-modelinfo)), which describes the
machine before it exists. A test checks, for every Mac model, that each slot of the table gives a
device of that device model.

Each machine's bus has `bus_ops(bus)`, which returns the object for the id of one of the
machine's own buses.

**A device can add buses to the machine**, as the Apple IIe Card adds the bus of its drives,
`Floppy:PDS_LC.0`. Most devices add none. One that does gives them with
`DeviceOps::extra_buses()`, each as a pair:

- a `Bus`, the description of the bus, with its slots and no device in them;
- the object that implements `BusOps` for it. For the card, that is `DiskII`, its disk controller.

So a device that describes a bus always provides it too. The emulator uses the pairs twice:

- **To build the list of buses.** It goes through the machine's own buses, and asks each device
  it finds for its extra buses. It puts their descriptions at the end of the list, and fills in
  their slots as for any bus.
- **To reach an added bus.** A machine knows nothing of the buses its devices add. The emulator
  first tells by the id whether a bus is one of the Mac model's. For the id of an added bus, it
  finds the device that added it, by asking the devices in the slots of the machine's buses for
  their extra buses, then asks that device again and takes the object of the pair with that id.

The device owns the buses it adds: the card's disk controller is part of the card, and the guest
reaches it through the card. So the emulator keeps no reference to an added bus, nor a record of
the device that added it: it works that out when it needs the bus.

For example, this is how the emulator runs `LoadMedia(bus, slot, path, read_only)`, which the
frontend sends when the user picks an image for a drive. `bus` and `slot` are the ids of the bus
and of the slot:

```rust
/// The device in a slot, seen through its function
fn device_function(&mut self, bus: &str, slot: &str) -> Result<FunctionMut<'_>> {
    let device = self
        .bus_ops(bus)                                  // The bus
        .context("This machine has no such bus")?
        .device(slot)                                  // The device in the slot
        .context("There is no device in this slot")?;
    Ok(device.function())                              // The device, as its function
}

pub(super) fn load_media(
    &mut self,
    bus: &str,
    slot: &str,
    path: &Path,
    read_only: bool,
) -> Result<()> {
    match self.device_function(bus, slot)? {
        FunctionMut::Floppy(floppy) => floppy.load(path, read_only),
        FunctionMut::Cdrom(cdrom) => cdrom.load(path),
        FunctionMut::HardDisk(disk) => disk.load(path),
        _ => bail!("This device takes no media"),
    }
}
```

The same code serves a Mac drive, an Apple IIe Card drive and a SCSI CD-ROM drive: none of their
device models is named.

**Only the emulator's device code calls `BusOps` and `DeviceOps`.** It uses them to ask for
statuses, to run the frontend's commands, to make a machine and for the debugger.
The code that runs for each access of the guest never goes through them: a NuBus access goes to
the card through its slot trait, and a machine reads its built-in devices as fields. So neither
trait adds a call to that code.

## How the frontend learns what the machine has

The emulator runs on its own thread and owns the devices. It sends the frontend **events**, the
variants of `EmulatorEvent`, over a channel: among them the bus list and the statuses. The frontend
sends it **emulator commands**, the variants of `EmulatorCommand`, over another channel. They are
not the commands a device gets on its bus, such as SCSI commands.

```
 frontend thread                                emulator thread
 ------------------------------------           -----------------------------------
 Workspace  --load-->  Frontend  --commands-->  Emulator   --BusOps-->   Devices
 (device    <--save--  (menus,   <--Buses-----  (devices.rs)
  configs)              dialogs) <--DeviceStatus--
```

Events and commands name a slot by the ids of its bus and of the slot, as workspaces and save
states do: `LoadMedia("SWIM", "1", path, read_only)`.

**The bus list** is built when the machine is made, and again whenever a device is added or
removed. Loading a disk doesn't change it, because the image is in the status. The emulator sends
it as `EmulatorEvent::Buses(Arc<Vec<Bus>>)`, which the frontend keeps and reads with no copy.

**A status** is what a device's `status()` returns: the struct of its function. The emulator keeps
a table of the last status it sent, one per slot of the bus list. Only the emulator touches it.

**The status pass** asks every device for its status, compares it with the table, and for each one
that differs, updates the table and sends `EmulatorEvent::DeviceStatus(bus, slot, status)`. The
comparison is done once, in one place: a device only returns its status. The pass runs every 500 ms
while the machine runs, at the end of every emulator command that concerns a device, and when the
bus list is built. A change the guest makes, such as ejecting a disk, shows within 500 ms.

**The two can't arrive out of order.** When the bus list is built, the emulator empties its table
and sends the new list, then runs a pass, which sends every status again, since none is in the
table. The frontend empties its own table when it gets a list. Both are on the same channel, so the
frontend always has the list a status belongs to. For example, when the user attaches a CD-ROM
drive at SCSI ID 3:

```
 frontend                                       emulator
    | -- AddDevice(SCSI 3, ScsiCdrom) ------------> |  makes the drive, puts it in SCSI slot 3
    | <-- Buses(new list) --------------------------|  empties its table of statuses
    |  empties its own table                        |
    | <-- DeviceStatus(SCSI #0, Some(HardDisk ..)) -|  pass over the new list: every slot
    | <-- DeviceStatus(SCSI #3, Some(Cdrom ..)) ----|  that has a device
    | <-- ... --------------------------------------|
    |                    (every 500 ms)             |
    | <-- DeviceStatus(SCSI #3, Some(Cdrom ..)) ----|  only when the status changed, such as
    |                                               |  when the guest ejects the disc
```

**The frontend keeps** the last bus list and a table of the last status of each slot, from which it
draws its menus. It never asks the emulator for a status.

**A new machine** sends its list and a full pass before the frontend gets it, and **a restored
machine** the same once its devices are back in their slots.

**Why events and not a shared block.** Events need no lock, so the emulator never waits for the
frontend, and the list and the statuses can't arrive out of order, as above. A device's status is
small, and only the ones that change are sent.

| Task | How |
| --- | --- |
| A menu shows a slot | The device's `name` if it gives one, else the bus's title and the slot's id (on the SCSI bus: both); then the device's status. No device model is named |
| A menu lists what a slot can take | `models_for(bus.kind, mac)`, with the name and function in each `ModelInfo` |
| The user attaches a device | `AddDevice(config)`, naming the device model the user picked |
| The user removes a device | `RemoveDevice(bus, slot)` |
| The user changes something | A command per method, such as `LoadMedia(bus, slot, path, read_only)` |
| The emulator runs a command | Slot → `BusOps` → device → `DeviceOps` → function → method |

A slot whose bus takes devices while the machine runs lets the frontend attach one, with a device
config built from the user's answers. The function tells the frontend what to ask: an image for a
hard disk, nothing or an image or a folder for a CD-ROM drive, nothing for an Ethernet adapter.
The frontend takes the device model from what `models_for()` lists for the slot, the one the user
picked in the menu, and passes it on: it hard-codes none.

## How a machine is created

A machine is made from two things: a `MachineConfig`, which describes the machine apart from its
devices, and a list of device configs, one per device. The frontend builds them the same way
however the machine comes to exist: from its Load ROM dialog, from a workspace or from its command
line. It gives both to `Emulator::new()`:

```rust
pub fn new(
    machine: &MachineConfig,
    devices: Vec<DeviceConfig>,
) -> Result<(Emulator, Vec<DisplayHandle>, Vec<RefusedDevice>)>

pub struct RefusedDevice {
    pub config: DeviceConfig,  // A device config the machine couldn't use
    pub reason: String,        // Why, in words for the user
}
```

A device config is a `DeviceConfig`:

```rust
pub struct DeviceConfig {
    pub bus: String,                // The id of its bus
    pub slot: String,               // The id of its slot on that bus
    pub model: Option<DeviceModel>, // Its device model. None: the device the machine has there.
    pub rom: Option<PathBuf>,       // Its ROM file, if its device model needs one
    pub function: FunctionConfig,   // The Config of its function
}
```

[frontend.md](frontend.md#describing-a-machine) describes both structs field by field.

For example, a Macintosh LC with a 12" RGB monitor, the Apple IIe Card and a hard disk is this
list:

| `bus` | `slot` | `model` | `function` |
| --- | --- | --- | --- |
| `Built-in` | `video` | None | `Display`: the `RGB12` monitor |
| `PDS_LC` | `0` | `AppleIIePdsCard` | `AppleIIeCard`: an `Apple525` drive on its disk port |
| `SCSI` | `0` | `ScsiHardDisk` | `HardDisk`: the image `system.img` |

`Emulator::new()` uses the list in four steps:

```
 device configs               1. check, complete    2. the machine        3. add_device()
 Built-in video {RGB12}  -->  kept  -------------->  LcIBus::new() makes
                                                     the built-in video
 PDS_LC AppleIIePdsCard  -->  kept  ------------------------------------> card in the slot,
                                                                          with its drive
 SCSI 0 ScsiHardDisk     -->  kept  ------------------------------------> disk at ID 0
 nothing for SWIM 1, 2   -->  added: MacSuperDrive  --------------------> the two drives
```

1. **The list is checked and completed.** Each device config is compared with what
   `MacModel::buses()` gives for the Mac model. One is refused when the Mac model has no such bus,
   or the bus no such slot; when an earlier one names the same slot; when its device model is
   unknown, or doesn't fit that bus on that Mac model; or when its Config is that of another
   function than its device model's. Then each slot in which the Mac model has a
   device without being asked, and that no device config names, gets a device config for that
   device model. In the example, this adds the LC's two floppy drives, a `MacSuperDrive` in `SWIM`
   slots `1` and `2`. No device exists yet.
2. **The machine is constructed**, by the constructor of its Mac model's bus, such as
   `LcIBus::new()`. Every such constructor has the same two arguments, the `MachineConfig` and the
   list, and returns a `Result`: an SE/30 can't be made without the ROM file of its video. The
   constructor makes the logic board, with its built-in devices. For each built-in device, it
   looks in the list for the device config of that slot of the `Built-in` bus, and reads there the
   Config of the device's function, and its ROM file: the LCs and the Quadras their monitor and
   video RAM, the SE/30 its video ROM. Every other slot of the machine is empty.
3. **Every other device is added**, in the order of the list, by `add_device()`: the function
   that also adds a device while the machine runs. It makes the device with the create function
   of its `ModelInfo`, and the bus puts it in its slot with `BusOps::attach()`
   ([Making a device](#making-a-device)). In the example, the two floppy drives, the Apple IIe
   Card and the hard disk are made here, and the card's create function makes the card's drive.
   A device config of the `Built-in` bus goes to `configure_device()` instead: its device exists,
   and is given what a Config can change, such as the link of a built-in Ethernet controller.
4. **The machine is checked for a screen.** A Mac with no video of its own and no display, such as
   a Mac II whose only card has nothing on its connector, isn't made: `Emulator::new()` returns an
   error.

So the machine's constructor makes the logic board, and everything that sits in a slot or on a
connector is made by its create function and attached: a floppy drive and a card as well as a SCSI
disk. No constructor has a card or a drive as an argument.

A device model that is `fixed`, such as a card or a floppy drive, is only accepted in step 3.
Once the machine is made, `add_device()` and `remove_device()` refuse it.

`add_device()` takes an empty slot only: a device config for a slot that has a device is an
error. The device's create function makes the device from the whole device config, the bus puts
it there, and `add_device()` then calls `configure_device()`. That function, private to the core,
gives a device that exists what a Config can change, with the methods of its function:
`EthernetFunction::set_link()` for the link of an Ethernet adapter, and
`CdromFunction::set_folder()` for the folder of a CD-ROM drive. Nothing else of a Config changes
once a device is made. While the machine runs, the frontend changes them with a command per
method, `EthernetSetLink` and `CdromSetFolder`. **A Config that can't be applied doesn't cost
the device:** a CD-ROM drive whose folder is missing is made empty, and a DaynaPORT whose link
can't start is made with its link down. The failure is reported like a device config that
couldn't be used.

What a machine couldn't use comes back as the list of refused device configs, each with its
reason: a device model this build doesn't know, a slot the Mac model doesn't have, a second device
config for a slot already in the list, the Config of another function, a file that can't be read,
a Config that failed. The
frontend shows them in one message. A refused device config doesn't stop the machine from being
made.

`emu.devices()` reports the machine as the list it can be made from again. It reads the list of
buses, and makes one device config per device, from the `Device` of its slot: its device model,
its ROM file and what its `DeviceOps::config()` returned. Every device config it makes names its
device model, that of a built-in device like any other. It leaves out a device that the Mac
model has there without being asked, with no ROM file and a Config in which nothing was chosen. A
machine made from that list reports the same list, which the tests check for each Mac model
family.

So the list is what the frontend gave, with the changes the user asked for while the machine ran:
a disk branched to another image file, another link for an Ethernet adapter, another folder for a
CD-ROM drive. When a command changes a Config, the core sends the list of buses again.

## The debugger

The debugger's peripherals window has a "Devices" group, built by walking the bus list: each bus,
each slot, the device model in it, its status, and what `DeviceOps::debug_properties()` gives of
the device, which is its own `Debuggable` properties. The chips keep their registers in their own
entries; the devices on their buses are only in this group. That includes the built-in devices,
such as a machine's own video: they are on the `Built-in` bus, so they are in this group and not
among the machine's chips. It is built only while the window is open.

## Workspaces

A workspace is the file in which the frontend keeps one machine. `frontend_egui` writes it as
JSON, with serde. The list of device configs is in it under `devices`, next to the fields for the
machine itself:

```json
{
  "model": "MacII",
  "rom_path": "macii.rom",
  "devices": [
    { "bus": "NuBus", "slot": "9", "model": "DisplayCard824", "rom": "8-24.rom",
      "function": { "Display": { "monitor": "RGB12" } } },
    { "bus": "SCSI", "slot": "0", "model": "ScsiHardDisk",
      "function": { "HardDisk": { "media": "a.img" } } },
    { "bus": "SCSI", "slot": "3", "model": "ScsiCdrom",
      "function": { "Cdrom": {} } }
  ]
}
```

Each item is a `DeviceConfig` as serde writes it: no part of the format is written by hand.
`function` has the name of the variant of `FunctionConfig`, then the fields of its Config. A field
that is `None` is left out.

Every device config in the file names its device model, the built-in video of an SE/30 like any
other:

```json
{ "bus": "Built-in", "slot": "video", "model": "SE30Video", "rom": "se30-video.rom",
  "function": { "Display": {} } }
```

A device config with no `model` is read all the same: it means what the Mac model has in that
slot. A floppy drive is only in the list if it isn't the Mac model's own, such as the 800K drive
of a Macintosh 128K. Bus ids, slot ids, device model names, and the names of the variants and
fields of the Config structs are the file format and must not change; menu texts come from `title`
and `name`, which can change.

**The Config structs exist in every build**, whatever its cargo features. A device model behind a
disabled feature isn't in `DeviceModel` on that build. So a workspace with an Ethernet adapter
loads in a build without Ethernet: that one device config can't be read, and is reported with a
message.

**Paths.** A device config has absolute paths, and the file has them relative to the workspace
file. The frontend converts them without knowing each Config struct, with
`DeviceConfig::convert_paths()`:

```rust
/// Replaces each path of this device config, its ROM file and those in the Config of its
/// function, by what `convert` returns for it
pub fn convert_paths(&mut self, convert: impl Fn(&Path) -> PathBuf);
```

The frontend gives the function that converts one path, and the core applies it to each: to make
the paths relative when it saves, and absolute when it loads.

**Loading.** Each device config is read on its own: one that can't be read is reported and
skipped, and the rest of the file loads. Old fields are turned into device configs, and the paths
are made absolute. The machine is made from the list. What it couldn't use is reported in one
message.

**Saving** writes `emu.devices()`, with its paths made relative to the workspace file. So a saved
workspace describes the machine as it runs: **a device config the machine couldn't use is gone
from the file once it is saved**.

| In the file | Result |
| --- | --- |
| A device model this build doesn't know, or not enabled | Reported, not used |
| No such bus or slot on this Mac model | Reported, not used |
| A second device config for a slot already in the list | Reported, not used |
| A bus that doesn't take that device model | Reported, not used |
| A hard disk image or a card ROM that can't be read | Reported, not used |
| A CD-ROM folder that is missing | The drive is made empty, and the folder is reported |
| A field that isn't known | Ignored |
| A device config that can't be read | Reported, and skipped when the file is read |

**Older workspaces** have other fields in place of `devices`. The frontend still reads them, never
writes them, and turns them into device configs when the file is loaded. Each old field, and each
device in it, is read on its own: one that can't be read is skipped, with a warning in the same
message as the refused device configs.

| Old field | Becomes |
| --- | --- |
| `disks`, `scsi_targets` | `SCSI` device configs. An Ethernet adapter gets a new Ethernet address, which the frontend asks of the core |
| `display_card_rom_path`, with `video_card` in `init_args` | A `NuBus` device config; one at `Built-in` / `video` on the SE/30 |
| `monitor` and `vram_size` in `init_args` | The `DisplayConfig` at `Built-in` / `video`; that of the video card on a Mac II |
| `pds_card` and `iie_card_settings` in `init_args` | A `PDS_LC` device config, with the `IieCardSettings` as its Config |
| `override_fdd_type` in `init_args` | A `SWIM` device config naming the drive's device model |
| `ethernet_link_type` | The `link` of each Ethernet adapter |

`floppy_images` isn't converted: when the workspace is loaded, its first image goes in the first
free drive.

## Save states

A save state holds the whole machine, serialized with serde: the logic board and every device in
every slot, each with its state. A device in a slot is a trait object, such as a
`Box<dyn ScsiTarget>`, which the `typetag` crate lets serde save and restore. The file is:

- The header: the version of the file format, the Mac model, the date, a screenshot, and for
  each hard disk the ids of its bus and slot and the size of its image.
- The machine's state.
- The image of each hard disk, one after the other.

A SCSI printer that is printing a page saves the page in the same list, after the machine's
state, through methods of `ScsiTarget` of its own.

The images are those of every device whose function is HardDisk, whatever its bus: the emulator
walks the list of buses and asks each for its image with `HardDiskFunction::savestate_image()`.

A restore deserializes the machine, which brings every device back in its slot, a hard disk with
no storage. Each disk image is then written to a temporary file, which the emulator gives its disk
with `HardDiskFunction::load_image()`, and the disk goes on from that file.
`Emulator::temp_files()` lists those files, for the frontend to remove. The frontend then gives
the machine the things of the host, as it does for a new machine
([frontend.md](frontend.md#what-belongs-to-the-host)).

| Device | After a restore |
| --- | --- |
| Hard disk | It runs on the temporary copy of its image, and reports that file as its image |
| Floppy drive | It has its disk, with the writes not yet saved to a file. It no longer knows which file the disk came from, and write-back is off |
| CD-ROM drive | It is empty |
| Ethernet adapter | It has the default link of the build (NAT in a build with NAT), not the one it had: the link isn't in a save state |
| Video | Each display gets a new renderer |

A state that names a device model this build doesn't have, such as an Ethernet adapter in a build
without Ethernet, can't be loaded.

The serial bridges aren't in a save state, as they aren't part of the machine
([frontend.md](frontend.md#what-belongs-to-the-host)): a restored machine has none.

## Why it is this way

- **A flat list of buses**, not a tree: the menus, the workspace and the debugger walk one list,
  and the id of a bus that a device adds says which slot that device is in.
- **Ids outside a bus, slot numbers only inside it**: the rest of the code knows a slot by one
  thing, its id, so nothing else can disagree with it, and a slot of the `Built-in` bus needs no
  number. Buses are found by their id too. The machines have one bus of each kind today;
  a second one would be another id.
- **One function per device model**, and functions as traits: the frontend has one code path per
  function, and a new device model of a known function needs no frontend code. A device that does
  two things would be a card that adds a bus with two devices on it.
- **A device config is typed by function, not by device model**: what is chosen for a device is
  the Config struct of its function, such as `DisplayConfig`. The functions are a short, closed
  list that the frontend has code for anyway, so a device config has nothing specific to a device
  model, and generic code (the Load ROM dialog, the workspace, `AddDevice`) carries the device
  config of any device model. A device model has no settings of its own beyond its ROM file.
- **`BusOps` and `DeviceOps` only for the emulator's device code**: the guest reaches a device
  through its slot trait and nothing else, and the emulator's device code has one way to reach
  every bus and every device. A struct
  per bus wrapping the devices was rejected: it would have moved the devices away from the code
  that uses them.
- **One trait for buses and one for devices**: a slot belongs to a bus, so it is the bus that says
  which device is in a slot, and that puts one in or removes it. What a device is and does is the
  same question on every bus, so each device answers it itself, and no bus has to repeat it.
- **Statuses as events**: see
  [How the frontend learns what the machine has](#how-the-frontend-learns-what-the-machine-has).
- **A video display creates its own renderer**: nothing has to be handed to a video display when
  it is made or when a save state is loaded, and no type depends on the kind of renderer. So a
  NuBus card is a trait object like a SCSI target, complete in its own file, and no list of cards
  has to name it.
- **Ids everywhere**: commands, events, workspaces and save states all name a slot the same way,
  by the ids of its bus and of the slot. A command can't reach the wrong slot because the bus list
  changed after it was sent, as it could with a position in the list.
