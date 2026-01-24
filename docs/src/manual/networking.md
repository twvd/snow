# Networking

Snow supports network connectivity via SCSI ethernet adapter emulation. This
is an experimental feature that emulates a Daynaport SCSI/Link adapter,
allowing the emulated Macintosh to communicate over a virtual network.

## Building with ethernet support

Ethernet support is an optional feature that must be enabled at compile time:

```bash
# With NAT networking (recommended)
cargo build -r --features ethernet_nat

# Device only (no host networking)
cargo build -r --features ethernet
```

## Attaching an ethernet adapter

To attach an ethernet adapter, go to 'Drives > SCSI #n' where 'n' is an
unused SCSI ID (0 to 6), then select 'Attach Ethernet controller'.

The adapter will appear in the menu as 'Ethernet' under the selected SCSI ID.

You can also configure ethernet in a workspace file:

```json
{
  "scsi_targets": [
    "Ethernet"
  ]
}
```

## Network configuration

When built with the `ethernet_nat` feature, Snow runs a NAT (Network Address
Translation) engine that provides a virtual network for the emulated Mac.

Configure the emulated Mac's network settings as follows:

| Setting     | Value         |
|-------------|---------------|
| IP address  | 10.0.0.x      |
| Gateway     | 10.0.0.1      |
| Subnet mask | 255.255.255.0 |
| DNS server  | 8.8.8.8       |

Where 'x' is any number from 2 to 254. Any public DNS server can be used.

## Driver software

The emulated adapter identifies as a Daynaport SCSI/Link. You will need
appropriate driver software installed in the emulated Mac to use the adapter.
Daynaport drivers were commonly distributed with the original hardware and
may be found in vintage software archives.

## What works

The NAT engine provides outbound connectivity for the emulated Mac:

- **TCP** - Outbound TCP connections (web browsing, FTP, telnet, etc.)
- **UDP** - Outbound UDP traffic
- **ARP** - Address resolution for the virtual network

This allows the emulated Mac to connect to the internet through the host's
network connection.

## Current limitations

The following features are not yet implemented:

- **ICMP** - Ping does not work
- **DHCP** - You must configure IP addresses manually
- **Inbound connections** - Only outbound connections are supported
- **Host network bridging** - No direct layer 2 bridge to host interfaces
