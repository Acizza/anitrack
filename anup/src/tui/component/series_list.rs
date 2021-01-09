use super::{Component, Draw};
use crate::tui::widget_util::{block, style, text};
use crate::tui::{CurrentAction, UIState};
use crate::Args;
use crate::{
    series::{LastWatched, LoadedSeries},
    tui::backend::Key,
};
use anime::remote::Status;
use anyhow::Result;
use crossterm::event::KeyCode;
use tui::backend::Backend;
use tui::layout::Rect;
use tui::style::Color;
use tui::terminal::Frame;
use tui::text::Span;
use tui::widgets::{List, ListItem, ListState};

pub struct SeriesList {
    list_state: ListState,
}

impl SeriesList {
    pub fn init(args: &Args, state: &mut UIState, last_watched: &LastWatched) -> Self {
        let selected = {
            let desired_series = args.series.as_ref().or_else(|| last_watched.get());

            match desired_series {
                Some(desired) => state
                    .series
                    .iter()
                    .position(|series| series.nickname() == desired)
                    .unwrap_or(0),
                None => 0,
            }
        };

        state.series.set_selected(selected);
        state.init_selected_series();

        Self {
            list_state: ListState::default(),
        }
    }

    fn series_text(series: &LoadedSeries) -> Span {
        match series {
            LoadedSeries::Complete(series) => {
                let color = match series.data.entry.status() {
                    Status::Watching | Status::Rewatching => Color::Blue,
                    Status::Completed => Color::Green,
                    Status::OnHold => Color::Yellow,
                    Status::Dropped => Color::Red,
                    Status::PlanToWatch => Color::Gray,
                };

                text::with_color(series.data.config.nickname.as_str(), color)
            }
            LoadedSeries::Partial(data, _) => {
                text::with_color(data.config.nickname.as_str(), Color::LightRed)
            }
            LoadedSeries::None(cfg, _) => text::with_color(cfg.nickname.as_str(), Color::LightRed),
        }
    }
}

impl Component for SeriesList {
    type State = UIState;
    type KeyResult = Result<()>;

    fn process_key(&mut self, key: Key, state: &mut Self::State) -> Self::KeyResult {
        match *key {
            KeyCode::Up | KeyCode::Down => {
                match *key {
                    KeyCode::Up => state.series.dec_selected(),
                    KeyCode::Down => state.series.inc_selected(),
                    _ => unreachable!(),
                }

                state.init_selected_series();
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl<B> Draw<B> for SeriesList
where
    B: Backend,
{
    type State = UIState;

    fn draw(&mut self, state: &Self::State, rect: Rect, frame: &mut Frame<B>) {
        let highlight_style = match &state.current_action {
            CurrentAction::Idle => style::italic().fg(Color::Green),
            _ => style::italic().fg(Color::DarkGray),
        };

        let series_names = state
            .series
            .iter()
            .map(Self::series_text)
            .map(ListItem::new)
            .collect::<Vec<_>>();

        let series_list = List::new(series_names)
            .block(block::with_borders("Series"))
            .style(style::fg(Color::White))
            .highlight_style(highlight_style)
            .highlight_symbol(">");

        self.list_state.select(Some(state.series.index()));

        frame.render_stateful_widget(series_list, rect, &mut self.list_state);
    }
}
