use crate::config::Config;
use crate::detect;
use crate::err::{self, Result};
use crate::file::SaveDir;
use anime::local::{EpisodeMap, EpisodeMatcher};
use anime::remote::{RemoteService, SeriesInfo, Status};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt};
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Deserialize, Serialize)]
pub struct Series {
    #[serde(skip)]
    pub nickname: String,
    #[serde(skip)]
    pub episodes: EpisodeMap,
    pub path: PathBuf,
    pub episode_matcher: EpisodeMatcher,
    pub info: SeriesInfo,
    pub entry: SeriesEntry,
    pub player_args: Vec<String>,
}

impl Series {
    pub fn from_args_and_remote<S, R>(
        args: &clap::ArgMatches,
        nickname: S,
        config: &Config,
        remote: &R,
    ) -> Result<Series>
    where
        S: Into<String>,
        R: RemoteService + ?Sized,
    {
        let nickname = nickname.into();

        // We should process as much local information as possible before sending requests to
        // the remote service to avoid potentially putting unnecessary load on the service should
        // any errors crop up.
        let path = match args.value_of("path") {
            Some(path) => PathBuf::from(path),
            None => detect::best_matching_folder(&nickname, &config.series_dir)?,
        };

        let matcher = match args.value_of("matcher") {
            Some(pattern) => episode_matcher_with_pattern(pattern)?,
            None => EpisodeMatcher::new(),
        };

        let episodes = EpisodeMap::parse(&path, &matcher)?;

        // Now we can request all of that juicy data from the remote service.
        let info = best_matching_series_info(remote, &nickname)?;
        let entry = SeriesEntry::from_remote(remote, &info)?;

        let series = Series {
            nickname,
            episodes,
            path,
            episode_matcher: matcher,
            info,
            entry,
            player_args: Vec::new(),
        };

        Ok(series)
    }

    pub fn episode_path(&self, episode: u32) -> Option<PathBuf> {
        if episode == 0 || episode > self.info.episodes {
            return None;
        }

        let episode_filename = self.episodes.get(&episode)?;

        let mut path = self.path.clone();
        path.push(episode_filename);
        path.canonicalize().ok()
    }

    pub fn play_episode_cmd(&self, episode: u32, config: &Config) -> Result<Command> {
        let episode_path = self
            .episode_path(episode)
            .context(err::EpisodeNotFound { episode })?;

        let mut cmd = Command::new(&config.episode.player);
        cmd.arg(episode_path);
        cmd.args(&config.episode.player_args);
        cmd.args(&self.player_args);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.stdin(Stdio::null());

        Ok(cmd)
    }

    pub fn save_path(id: anime::remote::SeriesID) -> PathBuf {
        let mut path = PathBuf::from(SaveDir::LocalData.path());
        path.push(id.to_string());
        path.set_extension("mpack");
        path
    }

    pub fn save(&self) -> Result<()> {
        let path = Series::save_path(self.info.id);

        if let Some(dir) = path.parent() {
            if !dir.exists() {
                fs::create_dir_all(dir).context(err::FileIO { path: path.clone() })?;
            }
        }

        let serialized = rmp_serde::to_vec(self).context(err::RMPEncode { path: path.clone() })?;
        fs::write(&path, serialized).context(err::FileIO { path })
    }

    pub fn load<S>(id: anime::remote::SeriesID, nickname: S) -> Result<Series>
    where
        S: Into<String>,
    {
        let path = Series::save_path(id);
        let file = File::open(&path).context(err::FileIO { path: path.clone() })?;

        let mut series: Series = rmp_serde::from_read(file).context(err::RMPDecode { path })?;
        series.nickname = nickname.into();
        series.episodes = EpisodeMap::parse(&series.path, &series.episode_matcher)?;

        Ok(series)
    }

    pub fn force_sync_changes_to_remote<R>(&mut self, remote: &R) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        if remote.is_offline() {
            return self.save();
        }

        remote.update_list_entry(self.entry.inner())?;

        self.entry.needs_sync = false;
        self.save()
    }

    pub fn sync_changes_to_remote<R>(&mut self, remote: &R) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        if !self.entry.needs_sync {
            return Ok(());
        }

        self.force_sync_changes_to_remote(remote)
    }

    pub fn force_sync_changes_from_remote<R>(&mut self, remote: &R) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        if remote.is_offline() {
            return Ok(());
        }

        match remote.get_list_entry(self.entry.id())? {
            Some(entry) => self.entry = SeriesEntry::from(entry),
            None => self.entry.needs_sync = false,
        };

        self.save()
    }

    pub fn sync_changes_from_remote<R>(&mut self, remote: &R) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        if self.entry.needs_sync {
            return Ok(());
        }

        self.force_sync_changes_from_remote(remote)
    }

    pub fn begin_watching<R>(&mut self, remote: &R, config: &Config) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        self.sync_changes_from_remote(remote)?;

        let entry = &mut self.entry;
        let last_status = entry.status();

        match last_status {
            Status::Watching | Status::Rewatching => {
                // There is an edge case where all episodes have been watched, but the status
                // is still set to watching / rewatching. Here we just start a rewatch
                if entry.watched_eps() >= self.info.episodes {
                    entry.set_status(Status::Rewatching, config);
                    entry.set_watched_eps(0);

                    if last_status == Status::Rewatching {
                        entry.set_times_rewatched(entry.times_rewatched() + 1);
                    }
                }
            }
            Status::Completed => {
                entry.set_status(Status::Rewatching, config);
                entry.set_watched_eps(0);
            }
            Status::PlanToWatch | Status::OnHold => entry.set_status(Status::Watching, config),
            Status::Dropped => {
                entry.set_status(Status::Watching, config);
                entry.set_watched_eps(0);
            }
        }

        self.sync_changes_to_remote(remote)
    }

    pub fn episode_completed<R>(&mut self, remote: &R, config: &Config) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        let entry = &mut self.entry;
        let new_progress = entry.watched_eps() + 1;

        if new_progress >= self.info.episodes {
            // The watched episode range is inclusive, so it's fine to bump the watched count
            // if we're at exactly at the last episode
            if new_progress == self.info.episodes {
                entry.set_watched_eps(new_progress);
            }

            return self.series_complete(remote, config);
        }

        entry.set_watched_eps(new_progress);
        self.sync_changes_to_remote(remote)
    }

    pub fn episode_regressed<R>(&mut self, remote: &R, config: &Config) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        let entry = &mut self.entry;
        entry.set_watched_eps(entry.watched_eps().saturating_sub(1));

        let new_status = match entry.status() {
            Status::Completed if entry.times_rewatched() > 0 => Status::Rewatching,
            Status::Rewatching => Status::Rewatching,
            _ => Status::Watching,
        };

        entry.set_status(new_status, config);
        self.sync_changes_to_remote(remote)
    }

    pub fn series_complete<R>(&mut self, remote: &R, config: &Config) -> Result<()>
    where
        R: RemoteService + ?Sized,
    {
        let entry = &mut self.entry;

        // A rewatch is typically only counted once the series is completed again
        if entry.status() == Status::Rewatching {
            entry.set_times_rewatched(entry.times_rewatched() + 1);
        }

        entry.set_status(Status::Completed, config);
        self.sync_changes_to_remote(remote)
    }
}

pub fn best_matching_series_info<R, S>(remote: &R, name: S) -> Result<SeriesInfo>
where
    R: RemoteService + ?Sized,
    S: AsRef<str>,
{
    let name = name.as_ref();

    let mut results = remote.search_info_by_name(name)?;
    let index = detect::best_matching_info(name, results.as_slice())
        .context(err::NoMatchingSeries { name })?;

    let info = results.swap_remove(index);
    Ok(info)
}

pub fn episode_matcher_with_pattern<S>(pattern: S) -> Result<EpisodeMatcher>
where
    S: AsRef<str>,
{
    let pattern = pattern
        .as_ref()
        .replace("{title}", "(?P<title>.+)")
        .replace("{episode}", r"(?P<episode>\d+)");

    match EpisodeMatcher::from_pattern(pattern) {
        Ok(matcher) => Ok(matcher),
        // We want to use a more specific error message than the one the anime library
        // provides
        Err(anime::Error::MissingCustomMatcherGroup { group }) => {
            Err(err::Error::MissingEpisodeMatcherGroup { group })
        }
        Err(err) => Err(err.into()),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SeriesEntry {
    entry: anime::remote::SeriesEntry,
    needs_sync: bool,
}

impl SeriesEntry {
    pub fn from_remote<R>(remote: &R, info: &SeriesInfo) -> Result<SeriesEntry>
    where
        R: RemoteService + ?Sized,
    {
        match remote.get_list_entry(info.id)? {
            Some(entry) => Ok(SeriesEntry::from(entry)),
            None => Ok(SeriesEntry::from(info.id)),
        }
    }

    #[inline(always)]
    pub fn inner(&self) -> &anime::remote::SeriesEntry {
        &self.entry
    }

    #[inline(always)]
    pub fn needs_sync(&self) -> bool {
        self.needs_sync
    }

    #[inline(always)]
    pub fn id(&self) -> u32 {
        self.entry.id
    }

    #[inline(always)]
    pub fn watched_eps(&self) -> u32 {
        self.entry.watched_eps
    }

    #[inline(always)]
    pub fn set_watched_eps(&mut self, watched_eps: u32) {
        self.entry.watched_eps = watched_eps;
        self.needs_sync = true;
    }

    #[inline(always)]
    pub fn score(&self) -> Option<u8> {
        self.entry.score
    }

    #[inline(always)]
    pub fn set_score(&mut self, score: Option<u8>) {
        self.entry.score = score;
        self.needs_sync = true;
    }

    #[inline(always)]
    pub fn status(&self) -> Status {
        self.entry.status
    }

    #[inline(always)]
    pub fn set_status(&mut self, status: Status, config: &Config) {
        match status {
            Status::Watching if self.start_date().is_none() => {
                self.entry.start_date = Some(Local::today().naive_local());
            }
            Status::Rewatching
                if self.start_date().is_none()
                    || (self.status() == Status::Completed && config.reset_dates_on_rewatch) =>
            {
                self.entry.start_date = Some(Local::today().naive_local());
            }
            Status::Completed
                if self.end_date().is_none()
                    || (self.status() == Status::Rewatching && config.reset_dates_on_rewatch) =>
            {
                self.entry.end_date = Some(Local::today().naive_local());
            }
            Status::Dropped if self.end_date().is_none() => {
                self.entry.end_date = Some(Local::today().naive_local());
            }
            _ => (),
        }

        self.entry.status = status;
        self.needs_sync = true;
    }

    #[inline(always)]
    pub fn times_rewatched(&self) -> u32 {
        self.entry.times_rewatched
    }

    #[inline(always)]
    pub fn set_times_rewatched(&mut self, times_rewatched: u32) {
        self.entry.times_rewatched = times_rewatched;
        self.needs_sync = true;
    }

    #[inline(always)]
    pub fn start_date(&self) -> Option<NaiveDate> {
        self.entry.start_date
    }

    #[inline(always)]
    pub fn end_date(&self) -> Option<NaiveDate> {
        self.entry.end_date
    }
}

impl From<anime::remote::SeriesEntry> for SeriesEntry {
    fn from(entry: anime::remote::SeriesEntry) -> SeriesEntry {
        SeriesEntry {
            entry,
            needs_sync: false,
        }
    }
}

impl From<u32> for SeriesEntry {
    fn from(id: u32) -> SeriesEntry {
        let remote_entry = anime::remote::SeriesEntry::new(id);
        SeriesEntry::from(remote_entry)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SavedSeries {
    pub last_watched_id: Option<anime::remote::SeriesID>,
    pub name_id_map: HashMap<String, anime::remote::SeriesID>,
}

impl SavedSeries {
    pub fn save_path() -> PathBuf {
        let mut path = PathBuf::from(SaveDir::LocalData.path());
        path.push("saved_series");
        path.set_extension("toml");
        path
    }

    pub fn save(&self) -> Result<()> {
        let path = SavedSeries::save_path();

        if let Some(dir) = path.parent() {
            if !dir.exists() {
                fs::create_dir_all(dir).context(err::FileIO { path: path.clone() })?;
            }
        }

        let serialized = toml::to_string(self).context(err::TomlEncode { path: path.clone() })?;
        fs::write(&path, serialized).context(err::FileIO { path })
    }

    pub fn load() -> Result<SavedSeries> {
        let path = SavedSeries::save_path();
        let contents = fs::read_to_string(&path).context(err::FileIO { path: path.clone() })?;

        let saved_series: SavedSeries =
            toml::from_str(&contents).context(err::TomlDecode { path })?;

        Ok(saved_series)
    }

    pub fn load_or_default() -> Result<SavedSeries> {
        match SavedSeries::load() {
            Ok(saved_series) => Ok(saved_series),
            Err(ref err) if err.is_file_nonexistant() => Ok(SavedSeries::default()),
            Err(err) => Err(err),
        }
    }

    pub fn load_series<S>(&self, nickname: S) -> Result<Series>
    where
        S: AsRef<str> + Into<String>,
    {
        let id = match self.name_id_map.get(nickname.as_ref()) {
            Some(&id) => id,
            None => {
                return Err(err::Error::NoMatchingSeries {
                    name: nickname.into(),
                })
            }
        };

        Series::load(id, nickname)
    }

    pub fn load_all_series_and_validate(&mut self) -> Result<Vec<Series>> {
        let mut results = Vec::with_capacity(self.name_id_map.len());

        self.name_id_map
            .retain(|name, &mut id| match Series::load(id, name) {
                Ok(series) => {
                    if !series.path.exists() {
                        return false;
                    }

                    results.push(series);
                    true
                }
                Err(_) => {
                    let data_path = Series::save_path(id);
                    fs::remove_file(data_path).ok();
                    false
                }
            });

        results.shrink_to_fit();
        Ok(results)
    }

    pub fn contains<S>(&self, nickname: S) -> bool
    where
        S: AsRef<str>,
    {
        self.name_id_map.contains_key(nickname.as_ref())
    }

    pub fn insert(&mut self, series: &Series) {
        self.name_id_map
            .insert(series.nickname.clone(), series.info.id);
    }

    pub fn set_last_watched(&mut self, id: anime::remote::SeriesID) -> bool {
        let is_different = match self.last_watched_id {
            Some(old_id) => old_id != id,
            None => true,
        };

        self.last_watched_id = Some(id);
        is_different
    }

    pub fn insert_and_save_from_args_and_remote<S, R>(
        &mut self,
        args: &clap::ArgMatches,
        nickname: S,
        config: &Config,
        remote: &R,
    ) -> Result<Series>
    where
        S: Into<String>,
        R: RemoteService + ?Sized,
    {
        let series = Series::from_args_and_remote(args, nickname, config, remote)?;

        // We should save the new series to disk before the saved series list, so we don't
        // potentially end up with dangling mapping should the new series fail to save.
        series.save()?;

        self.insert(&series);
        self.save()?;

        Ok(series)
    }
}