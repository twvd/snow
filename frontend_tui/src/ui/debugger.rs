use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::{Buffer, Rect, StatefulWidget, Widget},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use snow_core::{
    bus::Address,
    cpu_m68k::{cpu::Breakpoint, regs::RegisterFile},
    emulator::comm::EmulatorStatus,
    types::Long,
};

use super::DisassemblyListing;

#[derive(Copy, Clone)]
pub enum DebuggerWidgetEvent {
    LineUp,
    LineDown,
}

#[derive(Default)]
pub struct DebuggerWidgetState {
    selected_line: usize,
}

impl DebuggerWidgetState {
    pub fn transition(&mut self, event: DebuggerWidgetEvent) {
        match event {
            DebuggerWidgetEvent::LineUp => {
                self.selected_line = self.selected_line.saturating_sub(1);
            }
            DebuggerWidgetEvent::LineDown => {
                self.selected_line = self.selected_line.saturating_add(1);
            }
        }
    }

    pub fn get_selected_address(&self, disassembly: &DisassemblyListing) -> Address {
        disassembly[self.selected_line].addr
    }
}

pub struct DebuggerWidget<'a> {
    disassembly: &'a DisassemblyListing,
    emustatus: &'a EmulatorStatus,
    lastregs: &'a RegisterFile,
}

impl<'a> DebuggerWidget<'a> {
    pub fn new(
        disassembly: &'a DisassemblyListing,
        emustatus: &'a EmulatorStatus,
        lastregs: &'a RegisterFile,
    ) -> Self {
        Self {
            disassembly,
            emustatus,
            lastregs,
        }
    }
}

impl StatefulWidget for DebuggerWidget<'_> {
    type State = DebuggerWidgetState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(100), Constraint::Min(20)])
            .split(area);

        Paragraph::new(
            self.disassembly
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let style = if i == state.selected_line {
                        Style::default().black().on_white()
                    } else {
                        Style::default()
                    };

                    Line::from(vec![
                        if e.addr == self.emustatus.regs.pc {
                            Span::from("► ").style(style.light_green())
                        } else if self
                            .emustatus
                            .breakpoints
                            .contains(&Breakpoint::Execution(e.addr))
                        {
                            Span::from("• ").style(style.red().bold())
                        } else {
                            Span::from("  ")
                        },
                        Span::from(format!(":{:06X} ", e.addr)),
                        Span::from(format!("{:<16} ", e.raw_as_string())).style(style.dark_gray()),
                        Span::from(e.str.to_owned()),
                        Span::from(" "),
                    ])
                    .style(style)
                })
                .collect::<Vec<_>>(),
        )
        .block(Block::bordered().title("Disassembly"))
        .render(layout[0], buf);

        let layout_right = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Min(4),
                Constraint::Percentage(100),
                Constraint::Min(6),
            ])
            .split(layout[1]);

        Paragraph::new(vec![
            if self.emustatus.running {
                Line::from("Running").style(Style::default().light_green())
            } else {
                Line::from("Stopped").style(Style::default().red())
            },
            Line::from(vec![
                Span::from("Cycles ").style(Style::default().blue().bold()),
                Span::from(format!("{:>10}", self.emustatus.cycles))
                    .style(Style::default().white()),
            ]),
        ])
        .block(Block::bordered().title("CPU"))
        .render(layout_right[0], buf);
        let reg = |name, v: &dyn Fn(&RegisterFile) -> Long| {
            let style = if v(&self.emustatus.regs) != v(self.lastregs) {
                Style::default().light_yellow()
            } else {
                Style::default().gray()
            };
            Line::from(vec![
                Span::from(format!("{name:<4}")).style(Style::default().blue().bold()),
                Span::from(format!("{:08X}", v(&self.emustatus.regs))).style(style),
            ])
        };
        Paragraph::new(vec![
            reg("D0", &|r: &RegisterFile| r.read_d::<Long>(0)),
            reg("D1", &|r: &RegisterFile| r.read_d::<Long>(1)),
            reg("D2", &|r: &RegisterFile| r.read_d::<Long>(2)),
            reg("D3", &|r: &RegisterFile| r.read_d::<Long>(3)),
            reg("D4", &|r: &RegisterFile| r.read_d::<Long>(4)),
            reg("D5", &|r: &RegisterFile| r.read_d::<Long>(5)),
            reg("D6", &|r: &RegisterFile| r.read_d::<Long>(6)),
            reg("D7", &|r: &RegisterFile| r.read_d::<Long>(7)),
            Line::from(""),
            reg("A0", &|r: &RegisterFile| r.read_a::<Long>(0)),
            reg("A1", &|r: &RegisterFile| r.read_a::<Long>(1)),
            reg("A2", &|r: &RegisterFile| r.read_a::<Long>(2)),
            reg("A3", &|r: &RegisterFile| r.read_a::<Long>(3)),
            reg("A4", &|r: &RegisterFile| r.read_a::<Long>(4)),
            reg("A5", &|r: &RegisterFile| r.read_a::<Long>(5)),
            reg("A6", &|r: &RegisterFile| r.read_a::<Long>(6)),
            reg("A7", &|r: &RegisterFile| r.read_a::<Long>(7)),
            Line::from(""),
            reg("PC", &|r: &RegisterFile| r.pc),
            Line::from(""),
            reg("SSP", &|r: &RegisterFile| r.ssp),
            reg("USP", &|r: &RegisterFile| r.usp),
            Line::from(""),
            Line::from(vec![
                Span::from("SR  ").style(Style::default().blue().bold()),
                Span::from(format!("    {:04X}", self.emustatus.regs.sr.sr()))
                    .style(Style::default().white()),
            ]),
        ])
        .block(Block::bordered().title("Registers"))
        .render(layout_right[1], buf);

        let flag = |n, v| {
            Span::from(n).style(if v {
                Style::default().light_green()
            } else {
                Style::default().red()
            })
        };
        Paragraph::new(vec![
            Line::from(vec![
                flag("[C]", self.emustatus.regs.sr.c()),
                flag("[V]", self.emustatus.regs.sr.v()),
                flag("[Z]", self.emustatus.regs.sr.z()),
                flag("[N]", self.emustatus.regs.sr.n()),
                flag("[X]", self.emustatus.regs.sr.x()),
            ]),
            Line::from(""),
            Line::from(format!(
                "Int mask: {}",
                self.emustatus.regs.sr.int_prio_mask()
            )),
            Line::from(vec![
                flag("[SV]", self.emustatus.regs.sr.supervisor()),
                Span::from(" "),
                flag("[Trace]", self.emustatus.regs.sr.trace()),
                Span::from(" "),
            ]),
        ])
        .block(Block::bordered().title("Flags"))
        .render(layout_right[2], buf);
    }
}
