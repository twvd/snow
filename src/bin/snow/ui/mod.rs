use std::io::stdout;

use anyhow::Result;
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

enum View {
    Log,
    Debugger,
}

pub struct UserInterface {
    view: View,

    romfn: String,
    model: String,

    eventrecv: EmulatorEventReceiver,
    cmdsender: EmulatorCommandSender,

    emustatus: EmulatorStatus,
    lastregs: RegisterFile,
    disassembly: DisassemblyListing,

    state_log: TuiWidgetState,
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
            view: View::Debugger,
            state_log: TuiWidgetState::default(),
            romfn: romfn.to_string(),
            model: model.to_string(),
            eventrecv,
            cmdsender,

            emustatus,
            lastregs: RegisterFile::new(),
            disassembly: DisassemblyListing::new(),
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
        if event::poll(std::time::Duration::from_millis(16))? {
            if let event::Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(false),
                        KeyCode::PageUp => self.state_log.transition(TuiWidgetEvent::PrevPageKey),
                        KeyCode::PageDown => self.state_log.transition(TuiWidgetEvent::NextPageKey),
                        KeyCode::F(1) => self.view = View::Log,
                        KeyCode::F(2) => self.view = View::Debugger,
                        KeyCode::F(5) => self.cmdsender.send(EmulatorCommand::Run)?,
                        KeyCode::F(9) => self.cmdsender.send(EmulatorCommand::Step)?,
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
                .map(|e| {
                    Line::from(vec![
                        if e.addr == self.emustatus.regs.pc {
                            Span::from("► ").style(Style::default().light_green())
                        } else {
                            Span::from("  ")
                        },
                        Span::from(format!(":{:06X} ", e.addr)),
                        Span::from(format!("{:<16} ", e.raw_as_string()))
                            .style(Style::default().dark_gray()),
                        Span::from(e.str.to_owned()),
                    ])
                })
                .collect::<Vec<_>>(),
        )
        .block(Block::bordered().title("Disassembly"))
        .render(layout[0], buf);

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
        .block(Block::bordered().title("CPU"))
        .render(layout[1], buf);
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
        functions[4] = "Run";
        functions[8] = "Step";
        let mut fkeys = Vec::with_capacity(10 * 2);
        for (f, desc) in functions.into_iter().enumerate() {
            fkeys.push(format!("F{:<2}", f + 1).black().on_blue().bold());
            fkeys.push(format!("{desc:<5}").blue().on_black().bold());
        }

        Paragraph::new(Line::from(fkeys)).render(layout_main[2], buf);
    }
}
