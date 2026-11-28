# AI agents (MCP)

Snow has a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server. With it, an AI agent such as
Claude Code can operate the emulated Mac like a person: it looks at the screen, types, uses the mouse, inserts
media, and can look inside the 68k with the debugger.

The server is off by default.

## Turning it on

Start Snow with `--mcp`, or set the environment variable `SNOW_MCP=1`:

```shell
snowemu --mcp MySystem.snoww
```

Each Snow with MCP on opens a Unix socket in `/tmp/snow-<uid>/`, a folder that only your user can open. Several
Snows can run at the same time. Each one has a name: the workspace or ROM file name without extension, or the
value of `SNOW_MCP_NAME`.

The MCP server needs a Unix host (macOS or Linux).

## Connecting an agent

The agent starts `snowemu --mcp-bridge`. This is a stdio MCP server that finds the running Snows and passes each
tool call to the one the agent names. For Claude Code:

```shell
claude mcp add snow -s user -- /path/to/snowemu --mcp-bridge
```

On macOS, the path in the application bundle is `/Applications/Snow.app/Contents/MacOS/Snow`.

## Tools

| Tool | What it does |
|------|--------------|
| `vm-list`, `vm-wait` | List the running Snows; wait until one is up or gone |
| `vm-info` | Model, run state, speed, screen size, floppy drives and SCSI targets |
| `vm-power` | Pause, resume, reset or quit |
| `vm-speed` | Accurate, dynamic, uncapped, video or fast-forward |
| `screenshot` | PNG of the emulated screen, with crop and zoom |
| `wait-screen` | Wait until the screen changes, settles, or both |
| `key`, `type-text` | Key combinations (`cmd+o`, `option+e`) and text on the US layout |
| `mouse-move`, `mouse-click`, `mouse-drag`, `mouse-button` | Absolute mouse in screen pixels |
| `media` | Insert and eject floppies, attach disks, CD-ROMs and the Ethernet adapter, set the link |
| `clipboard-read` | Text on the guest's clipboard |
| `serial-write`, `serial-read`, `serial-expect` | The modem and printer ports |
| `steps` | Several tool calls in one |
| `debug-registers`, `debug-run`, `debug-step`, `debug-break` | Registers, run to an address, step, breakpoints on addresses and A-line traps |
| `debug-traps` | Record the system traps the guest calls |
| `memory-read`, `memory-write`, `disassemble` | Look at and change guest memory |

The keyboard, mouse and screen tools take a `then` argument. It waits for the screen and returns a screenshot in
the same call, for example `"then": {"until": "change-then-stable"}`.

The mouse tools need the 'Absolute' mouse mode (the default) and a running Mac OS, as the pointer position is
written into the Mac OS low memory globals.

<div class="warning">
An agent that can use the MCP tools has full control over the emulated machine, its disks and the media it can
reach on the host. Only turn MCP on for machines you want an agent to use.
</div>
