use std::{fs, path::PathBuf};

use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::{Buffer, Rect, StatefulWidget},
    style::{Style, Stylize},
    text::Text,
    widgets::{Block, Cell, HighlightSpacing, Row, Table, TableState},
};
use snow_floppy::loaders::{Autodetect, ImageType};

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
        Self {
            entries,
            target_drive,
            tablestate: TableState::default().with_selected(0),
        }
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
            }
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
            .constraints(vec![Constraint::Percentage(100)])
            .split(area);

        StatefulWidget::render(
            Table::new(rows, [Constraint::Percentage(100), Constraint::Min(5)])
                .header(
                    ["Filename", "Type"]
                        .into_iter()
                        .map(Cell::from)
                        .collect::<Row>()
                        .style(Style::default().black().on_blue().bold())
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
    }
}
