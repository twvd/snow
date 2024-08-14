use std::io::stdout;

use anyhow::{bail, Context, Result};
use log::*;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{event, ExecutableCommand};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Backend, Color, CrosstermBackend};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::Terminal;
use snow::bus::Address;
use snow::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow::cpu_m68k::regs::RegisterFile;
use snow::emulator::comm::{
    EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver, EmulatorStatus,
};
use snow::types::Long;
use tui_logger::{TuiLoggerLevelOutput, TuiLoggerWidget, TuiWidgetEvent, TuiWidgetState};

type DisassemblyListing = Vec<DisassemblyEntry>;

#[derive(Clone, Copy)]
enum View {
    Log,
    Debugger,
}

pub struct UserInterface {
    cmd: Option<String>,

    view: View,

    romfn: String,
    model: String,

    eventrecv: EmulatorEventReceiver,
    cmdsender: EmulatorCommandSender,

    emustatus: EmulatorStatus,
    lastregs: RegisterFile,
    disassembly: DisassemblyListing,

    state_log: TuiWidgetState,

    debug_sel: usize,
}

impl UserInterface {
    pub fn new(
        romfn: &str,
        model: &str,
        eventrecv: EmulatorEventReceiver,
        cmdsender: EmulatorCommandSender,
    ) -> Result<Self> {
        let Ok(EmulatorEvent::Status(emustatus)) = eventrecv.try_recv() else {
            panic!("Initial status message not received")
        };
        Ok(Self {
            cmd: None,

            view: View::Debugger,
            state_log: TuiWidgetState::default(),
            romfn: romfn.to_string(),
            model: model.to_string(),
            eventrecv,
            cmdsender,

            emustatus,
            lastregs: RegisterFile::new(),
            disassembly: DisassemblyListing::new(),

            debug_sel: 0,
        })
    }

    pub fn init_terminal() -> Result<Terminal<impl Backend>> {
        // Set up terminal for ratatui
        stdout().execute(EnterAlternateScreen)?;
        enable_raw_mode()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        terminal.clear()?;

        Ok(terminal)
    }

    pub fn draw(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        terminal.draw(|frame| {
            frame.render_widget(self, frame.size());
        })?;

        Ok(())
    }

    fn generate_disassembly(&mut self, pc: Address, code: Vec<u8>) -> Result<()> {
        self.disassembly = Vec::from_iter(Disassembler::from(&mut code.into_iter(), pc));

        Ok(())
    }

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<bool> {
        // Emulator events
        while let Ok(event) = self.eventrecv.try_recv() {
            match event {
                EmulatorEvent::Status(s) => {
                    self.lastregs = self.emustatus.regs.clone();
                    self.emustatus = s;
                }
                EmulatorEvent::NextCode((a, i)) => self.generate_disassembly(a, i)?,
            }
        }

        self.draw(terminal)?;

        // TUI events
        if event::poll(std::time::Duration::from_millis(50))? {
            if let event::Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if self.cmd.is_some() {
                        match key.code {
                            KeyCode::Char(c) => self.cmd.as_mut().unwrap().push(c),
                            KeyCode::Backspace => {
                                self.cmd.as_mut().unwrap().pop();
                            }
                            KeyCode::Enter => {
                                let cmd = self.cmd.take().unwrap();
                                if let Err(e) = self.handle_command(&cmd) {
                                    error!("Command failed: {:?}", e);
                                }
                            }
                            _ => (),
                        }
                    }

                    match (self.view, key.code) {
                        (_, KeyCode::Char('/')) => self.cmd = Some("".to_string()),
                        (_, KeyCode::F(10)) => return Ok(false),
                        (_, KeyCode::F(1)) => self.view = View::Log,
                        (_, KeyCode::F(2)) => self.view = View::Debugger,
                        (_, KeyCode::F(3)) => self
                            .cmdsender
                            .send(EmulatorCommand::InsertFloppy(Box::new([])))?,
                        (_, KeyCode::F(5)) if self.emustatus.running => {
                            self.cmdsender.send(EmulatorCommand::Stop)?
                        }
                        (_, KeyCode::F(5)) => self.cmdsender.send(EmulatorCommand::Run)?,
                        (_, KeyCode::F(9)) => self.cmdsender.send(EmulatorCommand::Step)?,
                        (View::Log, KeyCode::PageUp) => {
                            self.state_log.transition(TuiWidgetEvent::PrevPageKey)
                        }
                        (View::Log, KeyCode::PageDown) => {
                            self.state_log.transition(TuiWidgetEvent::NextPageKey)
                        }
                        (View::Log, KeyCode::Down) => {
                            self.state_log.transition(TuiWidgetEvent::DownKey)
                        }
                        (View::Log, KeyCode::Up) => {
                            self.state_log.transition(TuiWidgetEvent::UpKey)
                        }
                        (View::Log, KeyCode::End) => {
                            self.state_log.transition(TuiWidgetEvent::SpaceKey)
                        }
                        (View::Debugger, KeyCode::Up) => {
                            self.debug_sel = self.debug_sel.saturating_sub(1)
                        }
                        (View::Debugger, KeyCode::Down) => {
                            self.debug_sel = self.debug_sel.saturating_add(1)
                        }
                        (View::Debugger, KeyCode::F(7)) => {
                            let addr = self.disassembly[self.debug_sel].addr;
                            self.cmdsender
                                .send(EmulatorCommand::ToggleBreakpoint(addr))?;
                        }
                        _ => (),
                    }
                }
            }
        }

        Ok(true)
    }

    pub fn shutdown_terminal(_terminal: &mut Terminal<impl Backend>) -> Result<()> {
        stdout().execute(LeaveAlternateScreen)?;
        disable_raw_mode()?;

        Ok(())
    }

    fn draw_debugger(&mut self, area: Rect, buf: &mut Buffer) {
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(100), Constraint::Min(20)])
            .split(area);

        Paragraph::new(
            self.disassembly
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let style = if i == self.debug_sel {
                        Style::default().black().on_white()
                    } else {
                        Style::default()
                    };

                    Line::from(vec![
                        if e.addr == self.emustatus.regs.pc {
                            Span::from("► ").style(style.light_green())
                        } else if self.emustatus.breakpoints.contains(&e.addr) {
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
            let style = if v(&self.emustatus.regs) != v(&self.lastregs) {
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

    fn handle_command(&mut self, cmd: &str) -> Result<()> {
        let tokens = cmd.split(' ').collect::<Vec<_>>();
        match *tokens.first().context("Empty command")? {
            "b" => {
                let addr = Address::from_str_radix(
                    tokens
                        .get(1)
                        .context("Need address")?
                        .trim_start_matches("0x"),
                    16,
                )?;
                self.cmdsender
                    .send(EmulatorCommand::ToggleBreakpoint(addr))?;
                Ok(())
            }
            "loadbin" => {
                let addr = Address::from_str_radix(
                    tokens
                        .get(1)
                        .context("Need address")?
                        .trim_start_matches("0x"),
                    16,
                )?;
                let data = std::fs::read(tokens[2])?;
                self.cmdsender.send(EmulatorCommand::BusWrite(addr, data))?;
                Ok(())
            }
            _ => bail!("Unknown command"),
        }
    }
}

impl Widget for &mut UserInterface {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let layout_main = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Max(1),
                Constraint::Min(0),
                Constraint::Max(1),
                Constraint::Max(1),
            ])
            .split(area);

        Paragraph::new(Line::from(format!(
            "Snow - {} ({}) - {}",
            self.romfn,
            self.model,
            if self.emustatus.running {
                "running"
            } else {
                "stopped"
            }
        )))
        .style(Style::new().black().on_blue().bold())
        .centered()
        .render(layout_main[0], buf);

        match self.view {
            View::Log => {
                TuiLoggerWidget::default()
                    .style_error(Style::default().fg(Color::Red))
                    .style_debug(Style::default().fg(Color::Green))
                    .style_warn(Style::default().fg(Color::Yellow))
                    .style_trace(Style::default().fg(Color::Magenta))
                    .style_info(Style::default().fg(Color::Cyan))
                    .output_separator('|')
                    .output_timestamp(Some("%H:%M:%S".to_string()))
                    .output_level(Some(TuiLoggerLevelOutput::Abbreviated))
                    .output_target(true)
                    .output_file(false)
                    .output_line(false)
                    .state(&self.state_log)
                    .render(layout_main[1], buf);
            }
            View::Debugger => self.draw_debugger(layout_main[1], buf),
        }

        let mut functions = vec![""; 10];
        functions[0] = "Log";
        functions[1] = "Debug";
        functions[9] = "Quit";

        #[allow(clippy::single_match)]
        match self.view {
            View::Debugger => {
                if !self.emustatus.running {
                    functions[4] = "Run";
                    functions[6] = "Brkpt";
                    functions[8] = "Step";
                } else {
                    functions[4] = "Stop";
                }
            }
            _ => (),
        }
        let mut fkeys = Vec::with_capacity(10 * 2);
        for (f, desc) in functions.into_iter().enumerate() {
            fkeys.push(format!("F{:<2}", f + 1).black().on_blue().bold());
            fkeys.push(format!("{desc:<5}").blue().on_black().bold());
        }

        if let Some(s) = &self.cmd {
            Paragraph::new(Line::from(format!(" > {}", s)))
                .style(Style::default().on_black())
                .render(layout_main[2], buf);
        } else {
            Paragraph::new(Line::from(" > Type '/' to enter a command"))
                .style(Style::default().dark_gray().on_black())
                .render(layout_main[2], buf);
        }

        Paragraph::new(Line::from(fkeys))
            .style(Style::default().on_black())
            .render(layout_main[3], buf);
    }
}
