use std::{fs, path::PathBuf};

use ratatui::{
    layout::{Constraint, Direction, Layout, Margin},
    prelude::{Buffer, Rect, StatefulWidget, Widget},
    style::{Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState,
    },
};
use snow_floppy::{
    loaders::{Autodetect, FloppyImageLoader, ImageType},
    Floppy, FloppyMetadata, FloppyType, OriginalTrackType,
};

pub struct BrowserEntry {
    path: PathBuf,
    imgtype: ImageType,
}

#[derive(Copy, Clone)]
pub enum BrowserWidgetEvent {
    LineUp,
    LineDown,
}

#[derive(Default)]
pub struct BrowserWidgetState {
    entries: Vec<BrowserEntry>,
    pub target_drive: usize,
    tablestate: TableState,
    metadata: FloppyMetadata,
    floppytype: Option<FloppyType>,
    imagetype: Option<ImageType>,
    tracks: String,
    scrollbar: ScrollbarState,
}

impl BrowserWidgetState {
    pub fn new(target_drive: usize, path: &str) -> Self {
        let mut entries = if let Ok(dir) = fs::read_dir(path) {
            dir.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    Some((e, Autodetect::detect(&fs::read(p).ok()?).ok()?))
                })
                .map(|(e, imgtype)| BrowserEntry {
                    path: e.path(),
                    imgtype,
                })
                .collect()
        } else {
            vec![]
        };
        entries.sort_unstable_by_key(|e| e.path.file_name().unwrap().to_string_lossy().to_string());
        let mut result = Self {
            scrollbar: ScrollbarState::new(entries.len() - 1),
            entries,
            target_drive,
            tablestate: TableState::default().with_selected(0),
            ..Default::default()
        };
        result.update_metadata();
        result
    }

    pub fn get_selected(&self) -> Option<PathBuf> {
        if self.entries.is_empty() {
            return None;
        }
        Some(self.entries[self.tablestate.selected()?].path.clone())
    }

    pub fn transition(&mut self, event: BrowserWidgetEvent) {
        match event {
            BrowserWidgetEvent::LineUp => {
                if self.entries.is_empty() {
                    return;
                }

                if self.tablestate.selected().unwrap() == 0 {
                    self.tablestate.select(Some(self.entries.len() - 1));
                } else {
                    let select = self.tablestate.selected().unwrap_or(0).saturating_sub(1);
                    self.tablestate.select(Some(select));
                }
                self.update_metadata();
            }
            BrowserWidgetEvent::LineDown => {
                if self.entries.is_empty() {
                    return;
                }

                let select = self.tablestate.selected().unwrap_or(0) + 1;
                if select < self.entries.len() {
                    self.tablestate.select(Some(select));
                } else {
                    self.tablestate.select(Some(0));
                }
                self.update_metadata();
            }
        }
        if let Some(select) = self.tablestate.selected() {
            self.scrollbar = self.scrollbar.position(select);
        }
    }

    fn update_metadata(&mut self) {
        if self.entries.is_empty() {
            return;
        }

        self.imagetype = Some(self.entries[self.tablestate.selected().unwrap()].imgtype);
        let s = self.get_selected().unwrap();
        if let Ok(img) = Autodetect::load_file(s.as_os_str().to_str().unwrap()) {
            self.metadata = img.get_metadata();
            self.floppytype = Some(img.get_type());
            self.tracks = format!(
                "{}/{}/{}",
                img.count_original_track_type(OriginalTrackType::Flux),
                img.count_original_track_type(OriginalTrackType::Bitstream),
                img.count_original_track_type(OriginalTrackType::Sector),
            );
        } else {
            self.metadata = Default::default();
            self.floppytype = None;
        }
    }
}

pub struct BrowserWidget {}

impl BrowserWidget {
    pub fn new() -> Self {
        Self {}
    }
}

impl StatefulWidget for BrowserWidget {
    type State = BrowserWidgetState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let rows = state.entries.iter().map(|entry| {
            Row::new(vec![
                Cell::from(Text::from(
                    entry.path.file_name().unwrap().to_str().unwrap(),
                )),
                Cell::from(Text::from(entry.imgtype.to_string())),
            ])
        });
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(100), Constraint::Min(5)])
            .split(area);

        // Image list table
        StatefulWidget::render(
            Table::new(rows, [Constraint::Percentage(100), Constraint::Min(10)])
                .header(
                    ["Filename", "Type"]
                        .into_iter()
                        .map(Cell::from)
                        .collect::<Row>()
                        .style(Style::default().blue().bold())
                        .height(1),
                )
                .highlight_style(Style::default().black().on_gray())
                .highlight_spacing(HighlightSpacing::Always)
                .block(Block::bordered().title(format!(
                    "Select image to load in drive #{}",
                    state.target_drive + 1
                ))),
            layout[0],
            buf,
            &mut state.tablestate,
        );

        // Scrollbar
        StatefulWidget::render(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("░"))
                .track_style(Style::default().blue().on_black().bold())
                .thumb_style(Style::default().blue().on_blue().not_bold()),
            layout[0].inner(Margin {
                vertical: 1,
                horizontal: 1,
            }),
            buf,
            &mut state.scrollbar,
        );

        // Metadata
        let layout_meta = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(layout[1]);
        let meta_key = Style::default().blue().bold();
        let meta_value = Style::default().gray();
        Paragraph::new(vec![
            Line::from(vec![
                Span::from("Title        : ").style(meta_key),
                Span::from(state.metadata.get("title").map_or("", |v| v)).style(meta_value),
            ]),
            Line::from(vec![
                Span::from("Developer    : ").style(meta_key),
                Span::from(state.metadata.get("developer").map_or("", |v| v)).style(meta_value),
            ]),
            Line::from(vec![
                Span::from("Publisher    : ").style(meta_key),
                Span::from(state.metadata.get("publisher").map_or("", |v| v)).style(meta_value),
            ]),
        ])
        .render(layout_meta[0], buf);
        Paragraph::new(vec![
            Line::from(vec![
                Span::from("Disk #       : ").style(meta_key),
                Span::from(state.metadata.get("disk_number").map_or("", |v| v)).style(meta_value),
            ]),
            Line::from(vec![
                Span::from("Disk type    : ").style(meta_key),
                Span::from(state.floppytype.map_or("".to_string(), |v| v.to_string()))
                    .style(meta_value),
            ]),
            Line::from(vec![
                Span::from("Image type   : ").style(meta_key),
                Span::from(state.imagetype.unwrap().as_friendly_str()).style(meta_value),
            ]),
            Line::from(vec![
                Span::from("Tracks  F/B/S: ").style(meta_key),
                Span::from(&state.tracks).style(meta_value),
            ]),
        ])
        .render(layout_meta[1], buf);
    }
}
