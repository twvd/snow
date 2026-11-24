//! Debug tools: look inside the 68k while the guest runs

use super::hub::{Hub, NOTIFIER_BASE};
use super::tools::{Out, R, boolean, int, string};
use crate::consts::TRAPS;
use serde_json::Value;
use snow_core::bus::Address;
use snow_core::cpu_m68k::bus::InspectView;
use snow_core::cpu_m68k::cpu::Breakpoint;
use snow_core::cpu_m68k::disassembler::Disassembler;
use snow_core::emulator::comm::{EmulatorCommand, MemoryNotifierRange};
use std::fmt::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MAX_READ: u32 = 64 * 1024;

static NEXT_ID: AtomicU32 = AtomicU32::new(NOTIFIER_BASE);

/// An address: a number, or a string in hex ("4081AFF8", "0x4081AFF8", "$4081AFF8")
pub fn address(args: &Value, key: &str) -> R<Option<Address>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|v| Address::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| format!("'{}' is not a 32-bit address", key)),
        Some(Value::String(s)) => {
            let t = s.trim();
            let t = t
                .strip_prefix("0x")
                .or_else(|| t.strip_prefix("0X"))
                .or_else(|| t.strip_prefix('$'))
                .unwrap_or(t);
            Address::from_str_radix(t, 16)
                .map(Some)
                .map_err(|_| format!("'{}' must be a hex address, not '{}'", key, s))
        }
        Some(_) => Err(format!("'{}' must be a hex address", key)),
    }
}

fn view(args: &Value) -> R<InspectView> {
    match string(args, "view")?.unwrap_or("virtual") {
        "virtual" | "logical" => Ok(InspectView::Virtual),
        "physical" => Ok(InspectView::Physical),
        "program" => Ok(InspectView::Program),
        v => Err(format!(
            "Unknown view '{}': virtual, physical or program",
            v
        )),
    }
}

/// Reads guest memory; None for bytes that can't be read
pub fn read(hub: &Hub, addr: Address, len: u32, view: InspectView) -> R<Vec<Option<u8>>> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    hub.send(EmulatorCommand::MemoryNotifierSet(
        id,
        MemoryNotifierRange {
            addr,
            len,
            view,
            follow_pc: false,
        },
    ))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let r = loop {
        if let Some((_, data)) = hub.take_memory(id) {
            break Ok(data);
        }
        if Instant::now() >= deadline {
            break Err("The emulator did not answer the memory read".to_string());
        }
        hub.wait_change(Duration::from_millis(20));
    };
    let _ = hub.send(EmulatorCommand::MemoryNotifierRemove(id));
    r
}

fn trap_name(trap: u16) -> Option<&'static str> {
    // Toolbox traps ignore the auto-pop bit; OS traps the flag bits
    let key = if trap & 0x0800 != 0 {
        trap & 0xFBFF
    } else {
        trap & 0xF8FF
    };
    TRAPS
        .iter()
        .find(|(t, _)| *t == key || *t == trap)
        .map(|(_, n)| *n)
}

fn trap_by_name(name: &str) -> Option<u16> {
    let n = name.trim_start_matches('_');
    TRAPS
        .iter()
        .find(|(_, t)| t.eq_ignore_ascii_case(n))
        .map(|(t, _)| *t)
}

fn disasm(hub: &Hub, addr: Address, count: usize) -> R<String> {
    let bytes = read(
        hub,
        addr,
        (count * 10).min(4096) as u32,
        InspectView::Program,
    )?;
    let mut it = bytes.into_iter().map_while(|b| b);
    let mut out = String::new();
    for e in Disassembler::from(&mut it, addr).take(count) {
        let _ = write!(out, "{:08X}  {:<20} {}", e.addr, e.raw_as_string(), e.str);
        if e.is_linea()
            && let Some(n) = trap_name(e.opcode())
        {
            let _ = write!(out, "  ; _{}", n);
        }
        out.push('\n');
    }
    if out.is_empty() {
        out = format!("Can't read code at {:08X}\n", addr);
    }
    Ok(out)
}

fn registers_text(hub: &Hub) -> R<String> {
    hub.with_status(|st| {
        let st = st.ok_or("No status from the emulator yet")?;
        let r = &st.regs;
        let mut s = String::new();
        for i in 0..8 {
            let _ = write!(s, "D{}={:08X} ", i, r.d[i]);
        }
        s.push('\n');
        for i in 0..7 {
            let _ = write!(s, "A{}={:08X} ", i, r.a[i]);
        }
        let _ = writeln!(s, "A7={:08X}", r.read_a::<u32>(7));
        let _ = writeln!(
            s,
            "PC={:08X} SR={:04X} ({}) USP={:08X} ISP={:08X} MSP={:08X} VBR={:08X}",
            r.pc,
            r.sr.sr(),
            if r.sr.supervisor() {
                "supervisor"
            } else {
                "user"
            },
            r.usp,
            r.isp,
            r.msp,
            r.vbr
        );
        let _ = writeln!(
            s,
            "{}, cycles {}",
            if st.running { "running" } else { "stopped" },
            st.cycles
        );
        Ok(s)
    })
}

fn pc(hub: &Hub) -> R<Address> {
    hub.with_status(|st| {
        st.map(|s| s.regs.pc)
            .ok_or_else(|| "No status yet".to_string())
    })
}

fn running(hub: &Hub) -> bool {
    hub.with_status(|st| st.is_some_and(|s| s.running))
}

pub fn registers(hub: &Hub) -> R<Out> {
    let mut s = registers_text(hub)?;
    s.push('\n');
    s.push_str(&disasm(hub, pc(hub)?, 6)?);
    Ok(Out::text(s))
}

pub fn memory_read(hub: &Hub, args: &Value) -> R<Out> {
    let addr = address(args, "address")?.ok_or("'address' is missing")?;
    let len = int(args, "length")?
        .unwrap_or(256)
        .clamp(1, i64::from(MAX_READ)) as u32;
    let data = read(hub, addr, len, view(args)?)?;
    let mut s = String::new();
    for (row, chunk) in data.chunks(16).enumerate() {
        let _ = write!(s, "{:08X}  ", addr.wrapping_add(row as u32 * 16));
        for (i, b) in chunk.iter().enumerate() {
            match b {
                Some(b) => {
                    let _ = write!(s, "{:02X} ", b);
                }
                None => s.push_str("?? "),
            }
            if i == 7 {
                s.push(' ');
            }
        }
        for _ in chunk.len()..16 {
            s.push_str("   ");
        }
        s.push(' ');
        for b in chunk {
            s.push(match b {
                Some(c) if (0x20..0x7F).contains(c) => *c as char,
                Some(_) => '.',
                None => '?',
            });
        }
        s.push('\n');
    }
    Ok(Out::text(s))
}

pub fn memory_write(hub: &Hub, args: &Value) -> R<Out> {
    let addr = address(args, "address")?.ok_or("'address' is missing")?;
    let hex: String = string(args, "hex")?
        .ok_or("'hex' is missing")?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err("'hex' must hold whole bytes, e.g. \"4E71 4E71\"".into());
    }
    let data = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| "'hex' must be hex digits".to_string())?;
    let n = data.len();
    hub.send(EmulatorCommand::InspectWrite {
        addr,
        data,
        phys: boolean(args, "physical")?.unwrap_or(false),
    })?;
    Ok(Out::text(format!("Wrote {} bytes at {:08X}", n, addr)))
}

pub fn disassemble(hub: &Hub, args: &Value) -> R<Out> {
    let addr = match address(args, "address")? {
        Some(a) => a,
        None => pc(hub)?,
    };
    let count = int(args, "count")?.unwrap_or(20).clamp(1, 400) as usize;
    Ok(Out::text(disasm(hub, addr, count)?))
}

pub fn traps(hub: &Hub, args: &Value) -> R<Out> {
    match string(args, "action")?.unwrap_or("read") {
        "start" => {
            hub.send(EmulatorCommand::SetSystrapHistory(true))?;
            Ok(Out::text("Recording system traps; do the thing, then read"))
        }
        "stop" => {
            hub.send(EmulatorCommand::SetSystrapHistory(false))?;
            Ok(Out::text("Stopped recording system traps"))
        }
        "read" => {
            let count = int(args, "count")?.unwrap_or(100).clamp(1, 5000) as usize;
            let filter = string(args, "filter")?.map(str::to_ascii_lowercase);
            let all = hub.systraps();
            let mut lines: Vec<String> = all
                .iter()
                .map(|e| {
                    format!(
                        "{:08X}  {:04X}  {}",
                        e.pc,
                        e.trap,
                        trap_name(e.trap).map_or(String::new(), |n| format!("_{}", n))
                    )
                })
                .filter(|l| {
                    filter
                        .as_deref()
                        .is_none_or(|f| l.to_ascii_lowercase().contains(f))
                })
                .collect();
            let skip = lines.len().saturating_sub(count);
            lines.drain(..skip);
            Ok(Out::text(format!(
                "{} traps recorded{}, last {}:\nPC        TRAP\n{}",
                all.len(),
                if all.is_empty() {
                    " (use action start first)"
                } else {
                    ""
                },
                lines.len(),
                lines.join("\n")
            )))
        }
        a => Err(format!("Unknown action '{}': start, read or stop", a)),
    }
}

fn breakpoint(args: &Value) -> R<Breakpoint> {
    if let Some(a) = address(args, "address")? {
        return Ok(Breakpoint::Execution(a));
    }
    if let Some(t) = string(args, "trap")? {
        let op = trap_by_name(t)
            .or_else(|| {
                u16::from_str_radix(t.trim_start_matches("0x").trim_start_matches('$'), 16).ok()
            })
            .ok_or_else(|| format!("Unknown trap '{}'", t))?;
        return Ok(Breakpoint::LineA(op));
    }
    Err("Give 'address' or 'trap'".into())
}

fn breakpoints(hub: &Hub) -> Vec<Breakpoint> {
    hub.with_status(|st| st.map(|s| s.breakpoints.clone()).unwrap_or_default())
}

fn describe(bp: &Breakpoint) -> String {
    match bp {
        Breakpoint::LineA(op) => format!(
            "trap {:04X}{}",
            op,
            trap_name(*op).map_or(String::new(), |n| format!(" _{}", n))
        ),
        Breakpoint::Execution(a) => format!("address {:08X}", a),
        b => format!("{:X?}", b),
    }
}

pub fn breaks(hub: &Hub, args: &Value) -> R<Out> {
    let action = string(args, "action")?.unwrap_or("list");
    match action {
        "add" | "remove" => {
            let bp = breakpoint(args)?;
            let have = breakpoints(hub).contains(&bp);
            if have != (action == "add") {
                hub.send(EmulatorCommand::ToggleBreakpoint(bp))?;
                hub.wait_change(Duration::from_millis(200));
            }
            Ok(Out::text(format!(
                "{} breakpoint at {}",
                if action == "add" { "Set" } else { "Removed" },
                describe(&bp)
            )))
        }
        "clear" => {
            let all = breakpoints(hub);
            for bp in &all {
                hub.send(EmulatorCommand::ToggleBreakpoint(*bp))?;
            }
            Ok(Out::text(format!("Removed {} breakpoints", all.len())))
        }
        "list" => {
            let all = breakpoints(hub);
            Ok(Out::text(if all.is_empty() {
                "No breakpoints".to_string()
            } else {
                all.iter().map(describe).collect::<Vec<_>>().join("\n")
            }))
        }
        a => Err(format!(
            "Unknown action '{}': add, remove, clear or list",
            a
        )),
    }
}

/// Waits for the status to say running or not
fn wait_running(hub: &Hub, want: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while running(hub) != want {
        if Instant::now() >= deadline {
            return false;
        }
        hub.wait_change(Duration::from_millis(20));
    }
    true
}

pub fn step(hub: &Hub, args: &Value) -> R<Out> {
    if running(hub) {
        return Err("The CPU runs: stop it first (vm-power pause, or a breakpoint)".into());
    }
    let mode = string(args, "mode")?.unwrap_or("into");
    let count = int(args, "count")?.unwrap_or(1).clamp(1, 1000);
    for _ in 0..count {
        let before = hub.with_status(|st| st.map(|s| s.cycles));
        hub.send(match mode {
            "into" => EmulatorCommand::Step,
            "over" => EmulatorCommand::StepOver,
            "out" => EmulatorCommand::StepOut,
            m => return Err(format!("Unknown mode '{}': into, over or out", m)),
        })?;
        // Wait for the status after the step
        let deadline = Instant::now() + Duration::from_secs(5);
        while hub.with_status(|st| st.map(|s| s.cycles)) == before && Instant::now() < deadline {
            hub.wait_change(Duration::from_millis(10));
        }
        if mode != "into" {
            wait_running(hub, false, Duration::from_secs(10));
        }
    }
    registers(hub)
}

pub fn run(hub: &Hub, args: &Value) -> R<Out> {
    let temp = match address(args, "until")? {
        Some(a) => {
            let bp = Breakpoint::Execution(a);
            (!breakpoints(hub).contains(&bp)).then_some(bp)
        }
        None => None,
    };
    if let Some(bp) = temp {
        hub.send(EmulatorCommand::ToggleBreakpoint(bp))?;
    }
    hub.send(EmulatorCommand::Run)?;
    wait_running(hub, true, Duration::from_secs(2));
    let timeout = Duration::from_millis(
        int(args, "timeout-ms")?.map_or(10000, |v| v.clamp(0, 1_500_000) as u64),
    );
    let stopped = wait_running(hub, false, timeout);
    if let Some(bp) = temp {
        let _ = hub.send(EmulatorCommand::ToggleBreakpoint(bp));
    }
    if !stopped {
        return Ok(Out {
            text: vec![format!(
                "Not met: still running after {} ms (no breakpoint hit)",
                timeout.as_millis()
            )],
            unmet: true,
            ..Default::default()
        });
    }
    let mut out = registers(hub)?;
    out.text.insert(0, "Stopped".into());
    Ok(out)
}
