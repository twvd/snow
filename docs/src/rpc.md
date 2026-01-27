# Snow RPC Interface

Snow provides an optional RPC interface for external control of the emulator via JSON-RPC 2.0 over Unix domain sockets and TCP.

## Enabling RPC

Build Snow with the `rpc` feature enabled:

```bash
cargo build -r --features rpc
```

Start Snow with the `--rpc` flag to enable the RPC server:

```bash
./target/release/snow --rpc
```

The server will create a Unix socket at `$XDG_RUNTIME_DIR/snow-<PID>.sock` or `/tmp/snow-<PID>.sock`.

### Additional Options

- `--rpc-socket <PATH>` - Custom Unix socket path
- `--rpc-tcp <PORT>` - Also listen on a TCP port (for remote access)

## Protocol

Snow uses JSON-RPC 2.0. Each request is a JSON object on a single line, and the response is a JSON object on a single line.

### Example Request

```json
{"jsonrpc":"2.0","method":"status.get","id":1}
```

### Example Response

```json
{"jsonrpc":"2.0","result":{"running":true,"model":"Macintosh Plus",...},"id":1}
```

## API Reference

### Emulator Control

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `emulator.run` | - | `{ success }` | Resume emulator execution |
| `emulator.stop` | - | `{ success }` | Pause emulator execution |
| `emulator.reset` | - | `{ success }` | Reset the emulator |
| `emulator.get_cycles` | - | `{ cycles }` | Get current CPU cycle count |
| `emulator.programmer_key` | - | `{ success }` | Trigger programmer's key (power-on reset) |
| `input.release_all` | - | `{ success }` | Release all pressed keys/buttons |

### Status

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `status.get` | - | See below | Get current emulator status |

The `status.get` method returns a comprehensive status object:

```json
{
  "running": true,
  "model": "Macintosh IIcx",
  "cpu_type": "M68030",
  "ram_mb": 8,
  "screen": {
    "width": 640,
    "height": 480,
    "color": true
  },
  "has_adb": true,
  "has_scsi": true,
  "hd_floppy": true,
  "speed": "Accurate",
  "effective_speed": 1.0,
  "cycles": 1234567890,
  "scsi": [...],
  "floppy": [...],
  "serial": [...],
  "shared_dir": "/path/to/shared"
}
```

**Status fields:**
- `running` - Whether the emulator is currently executing
- `model` - Human-readable model name (e.g., "Macintosh Plus", "Macintosh IIcx")
- `cpu_type` - CPU type: "M68000", "M68020", or "M68030"
- `ram_mb` - RAM size in megabytes
- `screen` - Screen resolution and color information
  - `width` - Screen width in pixels
  - `height` - Screen height in pixels
  - `color` - Whether the display supports color
- `has_adb` - Whether the model has ADB (Apple Desktop Bus)
- `has_scsi` - Whether the model has SCSI
- `hd_floppy` - Whether the model supports HD (high-density) floppies
- `speed` - Current speed mode ("Accurate", "Uncapped", or "Video")
- `effective_speed` - Actual emulation speed multiplier
- `cycles` - Total CPU cycles executed
- `shared_dir` - Path to shared directory (or null if not set)

The `serial` array contains two entries for channels A and B:
```json
"serial": [
  { "channel": "A", "enabled": true, "status": "PTY: /dev/pts/3" },
  { "channel": "B", "enabled": false, "status": null }
]
```

### Speed Control

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `speed.get` | - | `{ mode, effective_speed }` | Get current speed mode |
| `speed.set` | `{ mode: "Accurate"\|"Uncapped"\|"Video" }` | `{ success, previous }` | Set speed mode |

### Screenshot

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `screenshot.get` | `{ format?: "png"\|"raw_rgba" }` | `{ width, height, data: base64, format }` | Get screenshot as base64-encoded data |
| `screenshot.save` | `{ path: string }` | `{ success, path }` | Save screenshot to file |

### Mouse Control

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `mouse.get_position` | - | `{ x, y }` | Get mouse position (if available) |
| `mouse.set_position` | `{ x, y }` | `{ success }` | Set absolute mouse position |
| `mouse.move` | `{ dx, dy }` | `{ success }` | Move mouse relative |
| `mouse.click` | `{ x?, y? }` | `{ success }` | Click at position (optional) |
| `mouse.button` | `{ state: "down"\|"up" }` | `{ success }` | Set mouse button state |

### Keyboard Control

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `keyboard.type` | `{ text, delay_ms?: 50 }` | `{ success }` | Type text string |
| `keyboard.combo` | `{ keys: ["command", "q"], delay_ms?: 50 }` | `{ success }` | Press key combination |
| `keyboard.key` | `{ key: string\|scancode, state: "down"\|"up" }` | `{ success }` | Press/release single key |
| `keyboard.release_all` | - | `{ success }` | Release all pressed keys |

### Floppy Disk

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `floppy.insert` | `{ drive: 0-2, path, write_protect?: false }` | `{ success }` | Insert floppy image |
| `floppy.eject` | `{ drive: 0-2 }` | `{ success }` | Eject floppy |

### CD-ROM

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `cdrom.insert` | `{ id: 0-6, path }` | `{ success }` | Insert CD-ROM image |
| `cdrom.eject` | `{ id: 0-6 }` | `{ success }` | Eject CD-ROM (may not be supported) |

### SCSI Devices

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `scsi.attach_hdd` | `{ id: 0-6, path }` | `{ success }` | Attach SCSI hard drive |
| `scsi.attach_cdrom` | `{ id: 0-6 }` | `{ success }` | Attach SCSI CD-ROM drive |
| `scsi.detach` | `{ id: 0-6 }` | `{ success }` | Detach SCSI target |

### Configuration

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `config.set_shared_dir` | `{ path: string\|null }` | `{ success }` | Set shared directory |
| `config.serial.get` | `{ channel: "A"\|"B" }` | `{ enabled, status }` | Get serial bridge status |
| `config.serial.enable` | `{ channel, mode: "pty"\|"tcp"\|"localtalk", port? }` | `{ success, status }` | Enable serial bridge |
| `config.serial.disable` | `{ channel }` | `{ success }` | Disable serial bridge |

### Debugger

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `debugger.step` | - | `{ success }` | Execute single instruction |
| `debugger.step_out` | - | `{ success }` | Run until function return |
| `debugger.step_over` | - | `{ success }` | Step over function call |
| `debugger.breakpoint.set` | `{ address, bp_type?: "exec"\|"read"\|"write" }` | `{ success }` | Set breakpoint |
| `debugger.breakpoint.list` | - | `{ breakpoints: [{ address, bp_type }] }` | Get all breakpoints |
| `debugger.breakpoint.remove` | `{ address, bp_type? }` | `{ success }` | Remove breakpoint |
| `debugger.breakpoint.toggle` | `{ address, bp_type? }` | `{ success, enabled }` | Toggle breakpoint |

Breakpoint types:
- `exec` (default) - Execution breakpoint
- `read` - Read watchpoint
- `write` - Write watchpoint

### Memory & Registers

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `memory.read` | `{ address, length?: 256 }` | `{ address, data: base64, length }` | Read memory |
| `memory.write` | `{ address, data: [bytes] }` | `{ success, bytes_written }` | Write memory |
| `registers.get` | `{ register?: string }` | `{ d0-d7, a0-a7, pc, sr, usp, ssp }` | Get register(s) |
| `registers.set` | `{ register, value }` | `{ success }` | Set register value |
| `disassembly.get` | `{ address?, count?: 20 }` | `{ entries: [{ address, bytes, mnemonic, operands }] }` | Get disassembly |

Valid register names: `d0`-`d7`, `a0`-`a7` (or `sp`), `pc`, `sr`, `usp`, `ssp`

### Audio

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `audio.get_mute` | - | `{ muted }` | Get mute status |
| `audio.set_mute` | `{ muted: bool }` | `{ success, muted }` | Mute/unmute audio |

### Recording & Playback

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `recording.start` | `{ path? }` | `{ success, path }` | Begin recording inputs |
| `recording.stop` | - | `{ success }` | End recording |
| `recording.status` | - | `{ recording, path }` | Check recording status |
| `recording.play` | `{ path }` | `{ success }` | Replay a recording |

### History & Tracing

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `history.instruction.enable` | `{ enabled }` | `{ success, enabled }` | Enable instruction logging |
| `history.instruction.get` | `{ count?: 100 }` | `{ entries: [...], enabled }` | Get instruction history |
| `history.systrap.enable` | `{ enabled }` | `{ success, enabled }` | Enable system trap logging |
| `history.systrap.get` | `{ count?: 100 }` | `{ entries: [...], enabled }` | Get systrap history |

Instruction history entry: `{ address, instruction, cycles }`
Systrap history entry: `{ address, trap_word, trap_name, cycles }`

### Peripheral Debug

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `peripheral.enable_debug` | `{ enabled }` | `{ success, enabled }` | Enable peripheral state logging |
| `peripheral.get_state` | - | `{ enabled, peripherals: [...] }` | Get all peripheral states |

Peripheral info: `{ name, properties: [{ key, value }] }`

Serial mode options:
- `pty` - Create a pseudo-terminal (Unix only), status shows PTY path (e.g., `/dev/pts/3`)
- `tcp` - Listen on TCP port (requires `port` parameter), status shows port and connection state
- `localtalk` - LocalTalk over UDP multicast for AppleTalk networking

### Window Control

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `window.get_fullscreen` | - | `{ fullscreen }` | Get fullscreen state |
| `window.set_fullscreen` | `{ fullscreen: bool }` | `{ success, fullscreen }` | Enter or exit fullscreen |
| `window.toggle_fullscreen` | - | `{ success, fullscreen }` | Toggle fullscreen mode |

## Key Name Reference

### Modifier Keys
- `command` / `cmd` / `apple` - Command/Apple key
- `option` / `alt` - Option key
- `control` / `ctrl` - Control key
- `shift` - Shift key
- `capslock` / `caps` - Caps Lock

### Function Keys
- `escape` / `esc`, `f1` through `f15`

### Special Keys
- `return` / `enter` - Return/Enter
- `tab` - Tab
- `space` - Space
- `backspace` - Backspace
- `delete` / `del` - Delete

### Arrow Keys
- `up`, `down`, `left`, `right`

### Navigation
- `home`, `end`, `pageup` / `pgup`, `pagedown` / `pgdn`, `insert` / `ins`

### Letters and Numbers
- `a` through `z` (lowercase)
- `0` through `9`

### Punctuation
- `-` / `minus`, `=` / `equals`
- `[` / `lbracket`, `]` / `rbracket`
- `\\` / `backslash`
- `;` / `semicolon`, `'` / `quote`
- `` ` `` / `grave` / `backtick`
- `,` / `comma`, `.` / `period`, `/` / `slash`

## Examples

### Using socat (Unix)

```bash
# Get status
echo '{"jsonrpc":"2.0","method":"status.get","id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Take screenshot
echo '{"jsonrpc":"2.0","method":"screenshot.save","params":{"path":"/tmp/screen.png"},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Type text
echo '{"jsonrpc":"2.0","method":"keyboard.type","params":{"text":"Hello, Mac!"},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Press Cmd+Q
echo '{"jsonrpc":"2.0","method":"keyboard.combo","params":{"keys":["command","q"]},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Insert floppy
echo '{"jsonrpc":"2.0","method":"floppy.insert","params":{"drive":0,"path":"/path/to/disk.img"},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Enable PTY serial on channel A
echo '{"jsonrpc":"2.0","method":"config.serial.enable","params":{"channel":"A","mode":"pty"},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Enable LocalTalk on channel B
echo '{"jsonrpc":"2.0","method":"config.serial.enable","params":{"channel":"B","mode":"localtalk"},"id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock

# Toggle fullscreen
echo '{"jsonrpc":"2.0","method":"window.toggle_fullscreen","id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock
```

### Python Client

```python
import socket
import json

def snow_rpc(method, params=None, sock_path="/tmp/snow.sock"):
    """Send an RPC request to Snow and return the result."""
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(sock_path)

    request = {
        "jsonrpc": "2.0",
        "method": method,
        "params": params or {},
        "id": 1
    }
    sock.send(json.dumps(request).encode() + b"\n")

    response = json.loads(sock.recv(1048576))
    sock.close()

    if "error" in response:
        raise Exception(response["error"]["message"])
    return response.get("result")

# Usage examples
status = snow_rpc("status.get")
print(f"Model: {status['model']} ({status['cpu_type']}, {status['ram_mb']} MB RAM)")
print(f"Screen: {status['screen']['width']}x{status['screen']['height']} {'color' if status['screen']['color'] else 'B&W'}")

# Check serial port status
for port in status['serial']:
    if port['enabled']:
        print(f"Serial {port['channel']}: {port['status']}")

snow_rpc("speed.set", {"mode": "Uncapped"})
snow_rpc("keyboard.combo", {"keys": ["command", "o"]})
snow_rpc("screenshot.save", {"path": "/tmp/screen.png"})
snow_rpc("floppy.insert", {"drive": 0, "path": "/path/to/disk.img"})
snow_rpc("emulator.reset")

# Enable PTY on modem port (channel A)
snow_rpc("config.serial.enable", {"channel": "A", "mode": "pty"})

# Toggle fullscreen
snow_rpc("window.toggle_fullscreen")
```

## Error Codes

Standard JSON-RPC 2.0 error codes are used:

| Code | Message | Description |
|------|---------|-------------|
| -32700 | Parse error | Invalid JSON |
| -32600 | Invalid Request | Invalid JSON-RPC request |
| -32601 | Method not found | Unknown method name |
| -32602 | Invalid params | Invalid method parameters |
| -32603 | Internal error | Internal server error |
