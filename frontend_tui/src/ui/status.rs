use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::{Buffer, Rect, Widget},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use snow_core::emulator::comm::EmulatorStatus;
use tui_logger::{TuiLoggerLevelOutput, TuiLoggerWidget};

pub struct StatusWidget<'a> {
    emustatus: &'a EmulatorStatus,
}

impl<'a> StatusWidget<'a> {
    pub fn new(emustatus: &'a EmulatorStatus) -> Self {
        Self { emustatus }
    }

    const ASCIIMAC: &'static [&'static str; 6] = &[
        "  ╒═════╕  ",
        "  │█████│  ",
        "  │▀▀▀▀▀│  ",
        "  │▪ ──═│  ",
        "  ╞═════╡  ",
        "  └────╨┘  ",
    ];
}

impl Widget for StatusWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Min(8),
                Constraint::Min(10),
                Constraint::Percentage(100),
            ])
            .split(area);

        let mut drivep = vec![];
        for (i, drive) in self
            .emustatus
            .fdd
            .iter()
            .enumerate()
            .filter(|(_, f)| f.present)
        {
            drivep.push(Line::from(vec![
                Span::from(format!(" #{} ", i + 1)).style(Style::default().blue().bold()),
                if drive.ejected {
                    Span::from(format!("no disk - press [{}] to load", i + 1))
                        .style(Style::default().dark_gray())
                } else {
                    Span::from(if drive.image_title.is_empty() {
                        "<no title>"
                    } else {
                        drive.image_title.as_str()
                    })
                    .style(Style::default().white())
                },
            ]));
            drivep.push(Line::from(vec![
                Span::from("     "),
                if drive.motor && !drive.writing {
                    Span::from(format!("Reading (track {})", drive.track))
                        .style(Style::default().blue())
                } else if drive.writing {
                    Span::from(format!("Writing (track {})", drive.track))
                        .style(Style::default().red())
                } else if drive.ejected {
                    Span::from(format!("Ejected (track {})", drive.track))
                        .style(Style::default().dark_gray())
                } else {
                    Span::from(format!("Stopped (track {})", drive.track))
                        .style(Style::default().gray())
                },
            ]));
            drivep.push(Line::from(""));
        }

        let layout_media = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(layout[1]);

        Paragraph::new(drivep)
            .block(Block::bordered().title("Floppy drives"))
            .render(layout_media[0], buf);

        if self.emustatus.model.has_scsi() {
            Paragraph::new(Vec::from_iter(self.emustatus.hdd.iter().enumerate().map(
                |(i, &d)| {
                    Line::from(vec![
                        Span::from(format!(" #{} ", i)).style(Style::default().blue().bold()),
                        if let Some(capacity) = d {
                            Span::from(format!(
                                "hdd{}.img ({:0.1} MB)",
                                i,
                                (capacity as f64) / 1024.0 / 1024.0
                            ))
                        } else {
                            Span::from("not present").dark_gray()
                        },
                    ])
                },
            )))
            .block(Block::bordered().title("Hard drives (SCSI)"))
            .render(layout_media[1], buf);
        }

        Paragraph::new(vec![
            Line::from(vec![
                Span::from(Self::ASCIIMAC[0]).white(),
                Span::from(format!(
                    "Motorola 68000 - {} KB RAM",
                    self.emustatus.model.ram_size() / 1024,
                )),
            ]),
            Line::from(Self::ASCIIMAC[1]).white(),
            Line::from(vec![
                Span::from(Self::ASCIIMAC[2]).white(),
                if self.emustatus.running {
                    Span::from("Running").style(Style::default().light_green())
                } else {
                    Span::from("Stopped").style(Style::default().red())
                },
            ]),
            Line::from(vec![
                Span::from(Self::ASCIIMAC[3]).white(),
                Span::from("Cycles ").style(Style::default().blue().bold()),
                Span::from(format!("{:>14}", self.emustatus.cycles))
                    .style(Style::default().white()),
            ]),
            Line::from(vec![
                Span::from(Self::ASCIIMAC[4]).white(),
                Span::from("Speed  ").style(Style::default().blue().bold()),
                Span::from(format!("{:>14}", self.emustatus.speed)).style(Style::default().white()),
            ]),
            Line::from(Self::ASCIIMAC[5]).white(),
        ])
        .block(Block::bordered().title(self.emustatus.model.to_string()))
        .render(layout[0], buf);

        TuiLoggerWidget::default()
            .style_error(Style::default().fg(Color::Red))
            .style_debug(Style::default().fg(Color::Green))
            .style_warn(Style::default().fg(Color::Yellow))
            .style_trace(Style::default().fg(Color::Magenta))
            .style_info(Style::default().fg(Color::Gray))
            .output_separator('|')
            .output_timestamp(Some("%H:%M:%S".to_string()))
            .output_level(Some(TuiLoggerLevelOutput::Abbreviated))
            .output_target(true)
            .output_file(false)
            .output_line(false)
            .block(Block::bordered().title("Log"))
            .render(layout[2], buf);
    }
}
