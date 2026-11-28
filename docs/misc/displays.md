# Displays

A **display** is a screen the frontend shows. Each display shows the frames of one video output
device: a video card, or the machine's built-in video. Such a device is called a **video display**
here. This document describes how a video display gives its frames to the frontend, how the
displays of a machine are found, and how the frontend arranges several of them. The devices
themselves are described in the [device model](device-model.md).

## A video display and its renderer

A video display is a device like any other, and Display is its function:

```rust
pub trait DisplayFunction {
    fn status(&self) -> DisplayStatus;
    fn frames(&self) -> FrameReceiver;   // Where the frontend picks up this display's frames
    fn blank(&mut self) -> Result<()>;   // Shows a blank screen, when the machine resets or stops
}
```

**A renderer is a helper that a video display creates for itself**, as a hard disk opens its own
image. It has two parts: the buffer of the frame being drawn, and a mailbox that holds one
finished frame at most. The frontend has the other end of the mailbox, the `FrameReceiver`, which
`frames()` gives.

- **When a video display is made, or loaded from a save state,** it creates its renderer.
- **When a machine is made, or a save state is loaded,** the emulator asks each video display for
  `frames()`, and gives the frontend a handle for each display.
- **At each frame of the guest,** when the vertical blank ends, the video display looks at the
  mailbox. If it is empty, it draws the frame into the renderer's buffer and tells the renderer it
  is ready. The renderer puts the buffer in the mailbox and starts a new one. If the frontend
  hasn't picked up the last frame yet, the video display draws nothing.
- **Each time it draws its window,** the frontend, on its own thread, takes the frame out of the
  mailbox, if there is one, and shows it.
- **When the machine resets or stops,** the emulator calls `blank()` on each video display.

**After a save state is loaded, a video shows its last picture**, so that a machine loaded while
it is paused shows its screens at once. A built-in video draws it when its machine is restored. A
video card leaves it in the mailbox when the emulator calls `frames()`, if the card had drawn a
frame before it was saved. A card that never drew, as in a machine that was just made, leaves the
mailbox empty.

## The displays of a machine

The displays of a machine are the devices in its bus list whose function is Display, in the order
of the list: the built-in video first, then the cards in the order of their slots.

The emulator remembers one thing about them: for each handle it gave the frontend, which display
it stands for, as the three values described below. It notes them when it makes the handles, so
the two always agree. With them it finds a display without going through the buses again, which
matters at each move of the mouse.

A display is made with its machine today: every device model with the Display function is
`fixed`, so the displays of a machine don't change while it runs.

A display is named by the ids of the bus and of the slot of the device that shows it, and by an
index, for a device that shows several displays. Every device shows one display today, with the
index 0.

The status of a Display device is plain data:

```rust
pub struct DisplayStatus {
    pub connected: bool,   // False: nothing is on its connector, so it shows no display
}
```

A video card or a built-in video with nothing on its connector, such as an 8-24 whose monitor is
`MacMonitor::NoMonitor`, is still a Display device, but it shows no display: its status says so,
and the frontend gets no handle for it. A machine needs at least one screen
([frontend.md](frontend.md#functions-what-to-ask-the-answers-and-the-status)).

`frames()` isn't in the status, because a status is made again at every status pass, while a
video display's end of the mailbox is set up once. The emulator calls `frames()` once per video
display, when a machine is made or a save state is loaded, and gives the frontend one handle per
display, in the order of the bus list:

```rust
pub struct DisplayHandle {
    pub name: String,            // "Built-in display", "NuBus slot D: Macintosh Display Card 8-24"
    pub bus: String,             // The device that shows it, by its bus and slot ids
    pub slot: String,
    pub index: usize,            // Which of the device's displays: 0 unless it shows several
    pub receiver: FrameReceiver, // Where its frames arrive
}
```

For example, a Quadra 700 with an 8-24 in slot `$D`:

| Display | Name | Bus / slot | Index |
| --- | --- | --- | --- |
| 0 | Built-in display | `Built-in` / `video` | 0 |
| 1 | NuBus slot D: Macintosh Display Card 8-24 | `NuBus` / `D` | 0 |

## Several displays

**This is experimental, and known to have issues.** The Load ROM dialog warns in red when a
machine has more than one display.

What Snow does with several displays depends on the mouse mode:

- **In absolute mouse mode**, Snow is allowed to assume that the guest runs Mac OS, and to read
  the guest's memory to interpret Mac OS's data structures. It walks Mac OS's list of displays to
  get the geometry of the desktop: how the displays are laid out relative to each other, and
  which ones are enabled. It reproduces the same layout in the frontend. This also lets the
  absolute mouse give the right coordinates on every display.
- **In relative mouse mode**, Snow can't assume Mac OS, so it falls back to a simple tiling of
  the displays.

### A more detailed description of the absolute mouse mode mechanism

The frontend draws every display, arranged as Mac OS arranges its screens, and sends the absolute
mouse to the display under the pointer:

```
 Monitors control panel (guest)           Snow's window (host)
 +---------+ +--------+                   +---------+ +--------+ +--------+
 | screen 1| |screen 2|        -->        | display | |display | |display |
 | (menu)  | |        |                   |    0    | |   1    | |   2    |  <- not used by the
 +---------+ +--------+                   +---------+ +--------+ +--------+     guest yet
```

**Where the guest puts its screens.** With several displays and the absolute mouse, the emulator
reads Color QuickDraw's list of screens from the guest's memory once a second. Each screen is a
GDevice record, which has the screen's rectangle and the reference number of its driver
(`gdRefNum`). The driver's record, an AuxDCE, says where the screen's video is, as Mac OS sees it:
the slot number (`dCtlSlot`) and the id of the slot resource (`dCtlSlotId`). Those two numbers are
the guest's, and only the machine knows what they mean, so the emulator asks the machine:
`display_from_Mac_AuxDCE()` takes them and gives the display: the ids of its bus and slot, and its
index, or None when they name no display of the machine. The id of the slot resource is what tells
apart the outputs of a card that has several, which the index then names. No card has several yet.

| Machine | `dCtlSlot` | Display |
| --- | --- | --- |
| Mac II family | `$9` to `$E` | The card in that `NuBus` slot |
| Quadra 700 and 800 | 0 | `Built-in` / `video` |
| Quadra 700 | `$D` and `$E` | The card in that `NuBus` slot |
| Quadra 800 | `$C` to `$E` | The card in that `NuBus` slot |

A machine answers for its built-in video itself. For any other slot number it calls a helper of
`Nubus`, which finds the card in the NuBus slot of that number and asks the card which of its
displays the slot resource is, with `NubusSlot::display_from_Mac_dCtlSlotId()`. A video card
answers with the index of that display, 0 for a card with one display. A card that shows nothing
keeps the default answer, None.

The emulator's own code does no arithmetic on slots or addresses, and the guest's numbers go no
further than the machine.

**How the frontend draws them.** It draws the displays the guest uses where the guest puts them,
scaled to fit the window. A position on any of them becomes a global position for the mouse. A
display the guest doesn't use yet, whose screen coordinates mean nothing, goes to the right of the
others and takes no mouse input; so does every display while the ROM runs, since its QuickDraw only
knows the startup screen. A row holds three displays at most: the displays the guest uses start the
first row, and the others follow, in rows under it.

### Known issues

- The absolute mouse is one event behind: the frontend takes the pointer position of the previous
  frame.
- The cursor lags more with several displays.
- Reading the guest's memory on a Mac II ignores the AMU, the chip that maps 24-bit addresses, so
  pointers in 24-bit mode are followed the way StripAddress does. Reading through the AMU would
  remove that.
- The mouse mode is read from each machine's bus; it should move to one common place.
- With a guest that isn't Mac OS, such as A/UX or Linux, nothing tells where the screens are: the
  displays could be arranged by hand.
- Scale, shaders and screenshots apply to the first display only.
