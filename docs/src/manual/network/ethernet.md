# Ethernet

Snow can emulate the DaynaPORT SCSI/Link Ethernet adapter which is an ethernet adapter with a SCSI interface. You
can use this adapter on any emulated Mac model that supports SCSI.

## Installation

To attach the emulated Ethernet adapter, use the 'Attach Ethernet adapter' option at any given SCSI ID in the menu:
'Drives' -> 'SCSI ID #_n_' -> 'Attach Ethernet adapter'.

<div class="warning">
Known issue: DaynaPORT drivers may not recognize the adapter. If you come across this, try to attach the Ethernet adapter
to SCSI ID 3 and your hard drive to ID 0.
</div>

![Attach Ethernet adapter](../../images/ethernet_attach.png)

Next, you will need to install the following in your emulated system:

* DaynaPORT drivers - the recommended (and tested) driver version is 7.5.3.
* MacTCP - 2.0.6 or higher is recommended.

### Installing DaynaPORT drivers

Use the Installer on the DaynaPORT driver disk. It will automatically select the correct driver for the adapter, which
is why you need to install the drivers _after_ attaching the Ethernet adapter (and rebooting).

![DaynaPORT drivers installer](../../images/ethernet_drivers1.png)

During the installation, you may see the warning below. You can simply ignore this by clicking 'Continue'.

![DaynaPORT installation warning](../../images/ethernet_drivers2.png)

Once the drivers are installed, reboot.

### Installing MacTCP

To install MacTCP, simply drag the extension and control panel onto the System folder in your emulated system. Reboot
afterwards.

## Ethernet links

Snow currently supports a NAT-based ethernet link that runs in userland on the host system. This link only supports
TCP and UDP connections.

You can select the Ethernet link type through the Drives menu: 'Drives' -> 'SCSI ID #_n_'.

![Ethernet link menu](../../images/ethernet_link.png)

### Configuring the emulated system for NAT

The NAT link emulates a gateway at IP-address 10.0.0.1. Your emulated system needs to have any IP-address in the
10.0.0.0/8 network.
There is no DNS or DHCP emulation, so you need to configure this manually and use an external DNS server (e.g. Google or
Cloudflare DNS).

To set this up in MacTCP, open the MacTCP control panel, select 'Ethernet Built-In' and click the 'More' button to get
to the following screen:

![MacTCP setup 2](../../images/ethernet_mactcp2.png)

In the 'Class' dropdown, select 'A'. Enter 10.0.0.1 as Gateway address in the bottom right input box. Enter DNS
information to use the DNS server of your choice. Then click 'OK'.

![MacTCP setup 1](../../images/ethernet_mactcp1.png)

In this screen, enter the IP-address for the emulated system, which needs to be in the 10.0.0.0/8 network (for example:
10.0.0.2). Close the MacTCP control panel and reboot the emulated system. You should now be able to go online within
the emulated system.

### Using tap interfaces (Linux only)

On Linux hosts, Snow can attach the virtual Ethernet adapter to a tap adapter, supporting all layer 2 and higher
protocols. This is a feature which allows advanced users to create more complex network setups and requires more
advanced networking and Linux knowledge. This also allows for the use of EtherTalk.

First, you need to create a tap interface that is owned by your user. For a tap interface to show up in Snow, the
interface name needs to start with either 'tap' or 'snow'. By making your user own the interface, you do not need to run
Snow as root for this to work.

For example, to create a tap interface owned by your current user:

```shell
sudo ip tuntap add dev tap0 mode tap user $UID
```

Then, you can select the tap interface in Snow through the menu: 'Drives' -> 'SCSI #_n_: Ethernet' -> 'TAP device:
tap_n_'.

![Selecting tap0 tap device](../../images/ethernet_tap.png)

For some examples of how to set up a network, see [Tap bridge network setups](../../guides/tapbridgesetups.md)

For troubleshooting and diagnostics of a tap-attached Ethernet adapter,
open ['View' -> 'Peripherals'](../debugging/peripherals.md)
and extend 'SCSI Controller', 'Targets', 'ID #_n_ - Ethernet'. This will show the interface status, MAC address of the
emulated adapter and some statistics on transmitted/received packets.

![Ethernet peripheral view](../../images/ethernet_debug.png)