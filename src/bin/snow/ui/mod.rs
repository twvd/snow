use std::io::stdout;

use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{event, ExecutableCommand};
use ratatui::layout::Rect;
use ratatui::prelude::{Backend, Color, CrosstermBackend};
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui::Terminal;
use tui_logger::{TuiLoggerLevelOutput, TuiLoggerWidget, TuiWidgetEvent, TuiWidgetState};

pub struct UserInterface {
    state_log: TuiWidgetState,
}

impl UserInterface {
    pub fn new() -> Result<Self> {
        Ok(Self {
            state_log: TuiWidgetState::default(),
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

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<bool> {
        self.draw(terminal)?;

        // TUI events
        if event::poll(std::time::Duration::from_millis(16))? {
            if let event::Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(false),
                        KeyCode::PageUp => self.state_log.transition(TuiWidgetEvent::PrevPageKey),
                        KeyCode::PageDown => self.state_log.transition(TuiWidgetEvent::NextPageKey),
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
}

impl Widget for &mut UserInterface {
    fn render(self, area: Rect, buf: &mut Buffer) {
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
            .render(area, buf);
    }
}
