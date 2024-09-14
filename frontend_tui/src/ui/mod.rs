mod debugger;

use std::io::stdout;

use anyhow::{bail, Context, Result};
use debugger::{DebuggerWidget, DebuggerWidgetEvent, DebuggerWidgetState};
use log::*;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{event, ExecutableCommand};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Backend, Color, CrosstermBackend, StatefulWidget};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Terminal;
use snow_core::bus::Address;
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::cpu_m68k::regs::RegisterFile;
use snow_core::emulator::comm::{
    EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver, EmulatorStatus,
};
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
    state_debugger: DebuggerWidgetState,
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
            state_log: TuiWidgetState::default(),
            state_debugger: DebuggerWidgetState::default(),

            cmd: None,

            view: View::Log,
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
        while event::poll(std::time::Duration::from_millis(0))? {
            let event::Event::Key(key) = event::read()? else {
                break;
            };

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
                    (_, KeyCode::F(5)) if self.emustatus.running => {
                        self.cmdsender.send(EmulatorCommand::Stop)?;
                    }
                    (_, KeyCode::F(5)) => self.cmdsender.send(EmulatorCommand::Run)?,
                    (_, KeyCode::F(9)) => self.cmdsender.send(EmulatorCommand::Step)?,
                    (View::Log, KeyCode::PageUp) => {
                        self.state_log.transition(TuiWidgetEvent::PrevPageKey);
                    }
                    (View::Log, KeyCode::PageDown) => {
                        self.state_log.transition(TuiWidgetEvent::NextPageKey);
                    }
                    (View::Log, KeyCode::Down) => {
                        self.state_log.transition(TuiWidgetEvent::DownKey);
                    }
                    (View::Log, KeyCode::Up) => {
                        self.state_log.transition(TuiWidgetEvent::UpKey);
                    }
                    (View::Log, KeyCode::End) => {
                        self.state_log.transition(TuiWidgetEvent::SpaceKey);
                    }
                    (View::Debugger, KeyCode::Up) => {
                        self.state_debugger.transition(DebuggerWidgetEvent::LineUp);
                    }
                    (View::Debugger, KeyCode::Down) => {
                        self.state_debugger
                            .transition(DebuggerWidgetEvent::LineDown);
                    }
                    (View::Debugger, KeyCode::F(7)) => {
                        let addr = self.state_debugger.get_selected_address(&self.disassembly);
                        self.cmdsender
                            .send(EmulatorCommand::ToggleBreakpoint(addr))?;
                    }
                    _ => (),
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

    fn handle_command(&self, cmd: &str) -> Result<()> {
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
            "dasm" => {
                let addr = Address::from_str_radix(
                    tokens
                        .get(1)
                        .context("Need address")?
                        .trim_start_matches("0x"),
                    16,
                )?;
                let len = tokens
                    .get(2)
                    .context("No length specified")?
                    .parse::<usize>()?;
                self.cmdsender
                    .send(EmulatorCommand::Disassemble(addr, len))?;
                Ok(())
            }
            "disk" | "disk1" => {
                let filename = tokens.get(1).context("No filename specified")?.to_string();
                self.cmdsender
                    .send(EmulatorCommand::InsertFloppy(0, filename))?;
                Ok(())
            }
            "disk2" => {
                let filename = tokens.get(1).context("No filename specified")?.to_string();
                self.cmdsender
                    .send(EmulatorCommand::InsertFloppy(1, filename))?;
                Ok(())
            }
            "writedisk" | "writedisk1" => {
                let filename = tokens.get(1).context("No filename specified")?.to_string();
                self.cmdsender
                    .send(EmulatorCommand::SaveFloppy(0, filename))?;
                Ok(())
            }
            "writedisk2" => {
                let filename = tokens.get(1).context("No filename specified")?.to_string();
                self.cmdsender
                    .send(EmulatorCommand::SaveFloppy(1, filename))?;
                Ok(())
            }
            "fps" => {
                let limit = tokens
                    .get(1)
                    .context("No argument specified")?
                    .parse()
                    .context("Argument must be integer")?;
                self.cmdsender.send(EmulatorCommand::SetFpsLimit(limit))?;
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
            View::Debugger => DebuggerWidget::new(
                &self.disassembly,
                &self.emustatus,
                &self.lastregs,
            )
            .render(layout_main[1], buf, &mut self.state_debugger),
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
