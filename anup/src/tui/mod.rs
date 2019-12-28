mod component;
mod ui;

use crate::config::Config;
use crate::err::{self, Result};
use crate::file::TomlFile;
use crate::series::database::Database as SeriesDatabase;
use crate::series::{self, LastWatched, Series};
use crate::try_opt_r;
use anime::remote::RemoteService;
use chrono::{DateTime, Duration, Utc};
use clap::ArgMatches;
use component::command_prompt::{Command, CommandPrompt};
use component::log::{LogItem, StatusLog};
use snafu::ResultExt;
use std::mem;
use std::process;
use termion::event::Key;
use ui::{Event, Events, UI};

pub fn run(args: &ArgMatches) -> Result<()> {
    let mut ui = UI::init()?;

    let mut cstate = {
        let config = Config::load_or_create()?;
        let remote = init_remote(args, &mut ui.status_log);
        let db = SeriesDatabase::open()?;

        CommonState { config, remote, db }
    };

    let mut ui_state = init_ui_state(&cstate, args)?;
    let events = Events::new(Duration::seconds(1));

    loop {
        ui.draw(&ui_state, cstate.remote.as_ref())?;
        ui.adjust_cursor(&ui_state)?;

        match events.next()? {
            Event::Input(key) => match key {
                // Exit
                Key::Char('q') if !ui_state.status_bar_state.in_input_dialog() => {
                    cstate.db.close().ok();
                    ui.clear().ok();
                    break Ok(());
                }
                key => match ui_state.process_key(&mut cstate, &mut ui.status_log, key) {
                    Ok(_) => (),
                    Err(err) => {
                        ui.status_log.push(LogItem::failed("Processing key", err));
                    }
                },
            },
            Event::Tick => match ui_state.process_tick(&cstate, &mut ui.status_log) {
                Ok(_) => (),
                Err(err) => ui.status_log.push(LogItem::failed("Processing tick", err)),
            },
        }
    }
}

fn init_remote(args: &ArgMatches, log: &mut StatusLog) -> Box<dyn RemoteService> {
    use anime::remote::anilist;
    use anime::remote::offline::Offline;

    match crate::init_remote(args, true) {
        Ok(remote) => remote,
        Err(err) => {
            match err {
                err::Error::NeedAniListToken => {
                    log.push(format!(
                        "No access token found. Go to {} \
                         and set your token with the 'token' command",
                        anilist::auth_url(crate::ANILIST_CLIENT_ID)
                    ));
                }
                _ => {
                    log.push(LogItem::failed("Logging in", err));
                    log.push(format!(
                        "If you need a new token, go to {} \
                         and set it with the 'token' command",
                        anilist::auth_url(crate::ANILIST_CLIENT_ID)
                    ));
                }
            }

            log.push("Continuing in offline mode");

            Box::new(Offline::new())
        }
    }
}

/// Items that are not tied to the UI and are commonly used together.
struct CommonState {
    config: Config,
    remote: Box<dyn RemoteService>,
    db: SeriesDatabase,
}

fn init_ui_state(cstate: &CommonState, args: &ArgMatches) -> Result<UIState> {
    let series = init_series_list(&cstate, args)?;
    let last_watched = LastWatched::load()?;

    let selected_series = {
        let desired_series = args
            .value_of("series")
            .map(|s| s.into())
            .or_else(|| last_watched.get().clone());

        match desired_series {
            Some(desired) => series
                .iter()
                .position(|series| series.nickname() == desired)
                .unwrap_or(0),
            None => 0,
        }
    };

    let mut ui_state = UIState {
        series,
        selected_series,
        last_watched,
        watch_state: WatchState::Idle,
        status_bar_state: StatusBarState::default(),
        last_used_command: None,
    };

    ui_state.ensure_cur_series_initialized(&cstate.db);
    Ok(ui_state)
}

fn init_series_list(cstate: &CommonState, args: &ArgMatches) -> Result<Vec<SeriesStatus>> {
    let series_names = series::database::get_series_names(&cstate.db)?;

    // Did the user specify a series that we don't have?
    let new_desired_series = args.value_of("series").and_then(|desired| {
        if series_names.contains(&desired.to_string()) {
            None
        } else {
            Some(desired)
        }
    });

    let mut series = series_names
        .into_iter()
        .map(SeriesStatus::Unloaded)
        .collect();

    // If we have the series, there's nothing left to do
    let desired_series = match new_desired_series {
        Some(desired_series) => desired_series,
        None => return Ok(series),
    };

    let params = crate::series_params_from_args(args);

    // Otherwise, we'll need to fetch & save it
    let new_series = Series::from_remote(
        desired_series,
        params,
        &cstate.config,
        cstate.remote.as_ref(),
    )
    .and_then(|series| {
        series.save(&cstate.db)?;
        Ok(series)
    });

    series.push(SeriesStatus::from_series(new_series, desired_series));
    Ok(series)
}

/// Current state of the UI.
pub struct UIState {
    series: Vec<SeriesStatus>,
    selected_series: usize,
    last_watched: LastWatched,
    watch_state: WatchState,
    status_bar_state: StatusBarState,
    last_used_command: Option<Command>,
}

impl UIState {
    fn cur_series_status(&self) -> Option<&SeriesStatus> {
        self.series.get(self.selected_series)
    }

    fn cur_valid_series(&self) -> Option<&Series> {
        self.cur_series_status()
            .and_then(|status| status.get_valid())
    }

    fn cur_series_status_mut(&mut self) -> Option<&mut SeriesStatus> {
        self.series.get_mut(self.selected_series)
    }

    fn cur_valid_series_mut(&mut self) -> Option<&mut Series> {
        self.cur_series_status_mut()
            .and_then(|status| status.get_valid_mut())
    }

    fn ensure_cur_series_initialized(&mut self, db: &SeriesDatabase) {
        let status = match self.cur_series_status() {
            Some(status) => status,
            None => return,
        };

        match status {
            SeriesStatus::Valid(_) | SeriesStatus::Invalid(_, _) => (),
            SeriesStatus::Unloaded(ref nickname) => {
                let new_status = {
                    let series = Series::load(db, nickname);
                    SeriesStatus::from_series(series, nickname)
                };

                // Unwrapping here is safe as we return early if the status is None earlier
                let status = self.cur_series_status_mut().unwrap();
                *status = new_status;
            }
        }
    }

    fn process_key(
        &mut self,
        state: &mut CommonState,
        log: &mut StatusLog,
        key: Key,
    ) -> Result<()> {
        if !self.is_idle() {
            return Ok(());
        }

        if self.status_bar_state.in_input_dialog() {
            return self.process_input_dialog_key(state, log, key);
        }

        match key {
            // Play next episode
            Key::Char(ch) if ch == state.config.tui.keys.play_next_episode => {
                let series = try_opt_r!(self.cur_valid_series());
                let nickname = series.config.nickname.clone();
                let is_diff_series = self.last_watched.set(nickname);

                if is_diff_series {
                    log.capture_status("Setting series as last watched", || {
                        self.last_watched.save()
                    });
                }

                log.capture_status("Playing next episode", || {
                    self.start_next_series_episode(&state)
                });
            }
            // Command prompt
            Key::Char(':') => {
                self.status_bar_state.set_to_command_prompt();
            }
            // Run last used command
            Key::Char(ch) if ch == state.config.tui.keys.run_last_command => {
                let cmd = match &self.last_used_command {
                    Some(cmd) => cmd.clone(),
                    None => return Ok(()),
                };

                self.process_command(cmd, state, log)?;
            }
            // Select series
            Key::Up | Key::Down => {
                self.selected_series = match key {
                    Key::Up => self.selected_series.saturating_sub(1),
                    Key::Down if self.selected_series < self.series.len().saturating_sub(1) => {
                        self.selected_series + 1
                    }
                    _ => self.selected_series,
                };

                self.ensure_cur_series_initialized(&state.db);
            }
            _ => (),
        }

        Ok(())
    }

    fn process_input_dialog_key(
        &mut self,
        state: &mut CommonState,
        log: &mut StatusLog,
        key: Key,
    ) -> Result<()> {
        match &mut self.status_bar_state {
            StatusBarState::Log => Ok(()),
            StatusBarState::CommandPrompt(prompt) => {
                use component::command_prompt::PromptResult;

                match prompt.process_key(key) {
                    Ok(PromptResult::Command(command)) => {
                        self.status_bar_state.reset();
                        self.last_used_command = Some(command.clone());
                        self.process_command(command, state, log)
                    }
                    Ok(PromptResult::Done) => {
                        self.status_bar_state.reset();
                        Ok(())
                    }
                    Ok(PromptResult::NotDone) => Ok(()),
                    // We need to set the status bar state back before propagating errors,
                    // otherwise we'll be stuck in the prompt
                    Err(err) => {
                        self.status_bar_state.reset();
                        Err(err)
                    }
                }
            }
        }
    }

    fn process_command(
        &mut self,
        command: Command,
        cstate: &mut CommonState,
        log: &mut StatusLog,
    ) -> Result<()> {
        match command {
            Command::Add(nickname, params) => {
                if self.series.iter().any(|s| s.nickname() == nickname) {
                    log.push("Series with the specified nickname already exists");
                    return Ok(());
                }

                if cstate.remote.is_offline() {
                    log.push("This command cannot be ran in offline mode");
                    return Ok(());
                }

                log.capture_status("Adding series", || {
                    let series = Series::from_remote(
                        &nickname,
                        params,
                        &cstate.config,
                        cstate.remote.as_ref(),
                    );

                    if let Ok(series) = &series {
                        series.save(&cstate.db)?;
                    }

                    let status = SeriesStatus::from_series(series, &nickname);

                    self.series.push(status);
                    self.series
                        .sort_unstable_by(|x, y| x.nickname().cmp(y.nickname()));

                    self.selected_series = self
                        .series
                        .iter()
                        .position(|series| series.nickname() == nickname)
                        .unwrap_or(0);

                    Ok(())
                });

                Ok(())
            }
            Command::Delete => {
                if self.selected_series >= self.series.len() {
                    return Ok(());
                }

                let series = self.series.remove(self.selected_series);
                let nickname = series.nickname();

                if self.selected_series == self.series.len() {
                    self.selected_series = self.selected_series.saturating_sub(1);
                }

                log.capture_status("Deleting series", || Series::delete(&cstate.db, nickname));

                self.ensure_cur_series_initialized(&cstate.db);
                Ok(())
            }
            Command::LoginToken(token) => {
                use anime::remote::anilist::AniList;
                use anime::remote::AccessToken;

                log.capture_status("Setting user access token", || {
                    let token = AccessToken::encode(token);
                    token.save()?;
                    cstate.remote = Box::new(AniList::authenticated(token)?);
                    Ok(())
                });

                Ok(())
            }
            Command::Matcher(pattern) => {
                use anime::local::{EpisodeMap, EpisodeMatcher};

                let series = try_opt_r!(self.cur_valid_series_mut());

                log.capture_status("Setting series episode matcher", || {
                    let matcher = match pattern {
                        Some(pattern) => series::episode_matcher_with_pattern(pattern)?,
                        None => EpisodeMatcher::new(),
                    };

                    series.episodes = EpisodeMap::parse(&series.config.path, &matcher)?;
                    series.config.episode_matcher = matcher;
                    series.save(&cstate.db)
                });

                Ok(())
            }
            Command::PlayerArgs(args) => {
                let series = try_opt_r!(self.cur_valid_series_mut());

                log.capture_status("Saving player args for series", || {
                    series.config.player_args = args;
                    series.save(&cstate.db)
                });

                Ok(())
            }
            Command::Progress(direction) => {
                use component::command_prompt::ProgressDirection;

                let series = try_opt_r!(self.cur_valid_series_mut());
                let remote = cstate.remote.as_ref();

                match direction {
                    ProgressDirection::Forwards => {
                        log.capture_status("Forcing forward watch progress", || {
                            series.episode_completed(remote, &cstate.config, &cstate.db)
                        });
                    }
                    ProgressDirection::Backwards => {
                        log.capture_status("Forcing backwards watch progress", || {
                            series.episode_regressed(remote, &cstate.config, &cstate.db)
                        });
                    }
                }

                Ok(())
            }
            Command::SyncFromRemote => {
                let series = try_opt_r!(self.cur_valid_series_mut());
                let remote = cstate.remote.as_ref();

                log.capture_status("Syncing entry from remote", || {
                    series.entry.force_sync_from_remote(remote)?;
                    series.save(&cstate.db)
                });

                Ok(())
            }
            Command::SyncToRemote => {
                let series = try_opt_r!(self.cur_valid_series_mut());
                let remote = cstate.remote.as_ref();

                log.capture_status("Syncing entry to remote", || {
                    series.entry.force_sync_to_remote(remote)?;
                    series.save(&cstate.db)
                });

                Ok(())
            }
            Command::Score(raw_score) => {
                let series = try_opt_r!(self.cur_valid_series_mut());

                let score = match cstate.remote.parse_score(&raw_score) {
                    Some(score) if score == 0 => None,
                    Some(score) => Some(score),
                    None => {
                        log.push(LogItem::failed("Parsing score", None));
                        return Ok(());
                    }
                };

                let remote = cstate.remote.as_ref();

                log.capture_status("Setting score", || {
                    series.entry.set_score(score);
                    series.entry.sync_to_remote(remote)?;
                    series.save(&cstate.db)
                });

                Ok(())
            }
            Command::Status(status) => {
                let series = try_opt_r!(self.cur_valid_series_mut());
                let remote = cstate.remote.as_ref();

                log.capture_status(format!("Setting series status to \"{}\"", status), || {
                    series.entry.set_status(status, &cstate.config);
                    series.entry.sync_to_remote(remote)?;
                    series.save(&cstate.db)
                });

                Ok(())
            }
        }
    }

    fn process_tick(&mut self, state: &CommonState, log: &mut StatusLog) -> Result<()> {
        match &mut self.watch_state {
            WatchState::Idle => (),
            WatchState::Watching(_, child) => {
                let status = match child.try_wait().context(err::IO)? {
                    Some(status) => status,
                    None => return Ok(()),
                };

                // The watch state should be set to idle immediately to avoid a potential infinite loop.
                let progress_time = match mem::replace(&mut self.watch_state, WatchState::Idle) {
                    WatchState::Watching(progress_time, _) => progress_time,
                    WatchState::Idle => unreachable!(),
                };

                let series = try_opt_r!(self.cur_valid_series_mut());

                if !status.success() {
                    log.push("Player did not exit properly");
                    return Ok(());
                }

                if Utc::now() >= progress_time {
                    log.capture_status("Marking episode as completed", || {
                        series.episode_completed(state.remote.as_ref(), &state.config, &state.db)
                    });
                } else {
                    log.push("Not marking episode as completed");
                }
            }
        }

        Ok(())
    }

    fn start_next_series_episode(&mut self, state: &CommonState) -> Result<()> {
        let series = try_opt_r!(self.cur_valid_series_mut());

        series.begin_watching(state.remote.as_ref(), &state.config, &state.db)?;

        let next_ep = series.entry.watched_eps() + 1;

        let child = series
            .play_episode_cmd(next_ep, &state.config)?
            .spawn()
            .context(err::FailedToPlayEpisode { episode: next_ep })?;

        let progress_time = {
            let secs_must_watch =
                (series.info.episode_length as f32 * state.config.episode.pcnt_must_watch) * 60.0;
            let time_must_watch = Duration::seconds(secs_must_watch as i64);

            Utc::now() + time_must_watch
        };

        self.watch_state = WatchState::Watching(progress_time, child);
        Ok(())
    }

    fn is_idle(&self) -> bool {
        self.watch_state == WatchState::Idle
    }
}

type Nickname = String;
type Reason = String;

enum SeriesStatus {
    Valid(Box<Series>),
    Invalid(Nickname, Reason),
    Unloaded(Nickname),
}

impl SeriesStatus {
    fn from_series<S>(series: Result<Series>, nickname: S) -> SeriesStatus
    where
        S: Into<String>,
    {
        match series {
            Ok(series) => SeriesStatus::Valid(Box::new(series)),
            // We want to use a somewhat concise error message here, so
            // we should strip error wrappers that don't provide much context
            Err(err::Error::Anime { source, .. }) => {
                SeriesStatus::Invalid(nickname.into(), format!("{}", source))
            }
            Err(err) => SeriesStatus::Invalid(nickname.into(), format!("{}", err)),
        }
    }

    fn get_valid(&self) -> Option<&Series> {
        match self {
            SeriesStatus::Valid(series) => Some(&series),
            SeriesStatus::Invalid(_, _) => None,
            SeriesStatus::Unloaded(_) => None,
        }
    }

    fn get_valid_mut(&mut self) -> Option<&mut Series> {
        match self {
            SeriesStatus::Valid(series) => Some(series),
            SeriesStatus::Invalid(_, _) => None,
            SeriesStatus::Unloaded(_) => None,
        }
    }

    fn nickname(&self) -> &str {
        match self {
            SeriesStatus::Valid(series) => series.config.nickname.as_ref(),
            SeriesStatus::Invalid(nickname, _) => nickname.as_ref(),
            SeriesStatus::Unloaded(nickname) => nickname.as_ref(),
        }
    }
}

enum StatusBarState {
    Log,
    CommandPrompt(CommandPrompt),
}

impl StatusBarState {
    fn set_to_command_prompt(&mut self) {
        *self = StatusBarState::CommandPrompt(CommandPrompt::new());
    }

    fn reset(&mut self) {
        *self = StatusBarState::default();
    }

    fn in_input_dialog(&self) -> bool {
        match self {
            StatusBarState::Log => false,
            StatusBarState::CommandPrompt(_) => true,
        }
    }
}

impl<'a> Default for StatusBarState {
    fn default() -> Self {
        StatusBarState::Log
    }
}

type ProgressTime = DateTime<Utc>;

#[derive(Debug)]
enum WatchState {
    Idle,
    Watching(ProgressTime, process::Child),
}

impl PartialEq for WatchState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (WatchState::Idle, WatchState::Idle) => true,
            (WatchState::Watching(_, _), WatchState::Watching(_, _)) => true,
            _ => false,
        }
    }
}
