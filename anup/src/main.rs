mod config;
mod err;
mod file;
mod series;
mod tui;
mod util;

use crate::config::Config;
use crate::err::Result;
use crate::file::TomlFile;
use crate::series::database::{Database as SeriesDatabase, Insertable, Selectable};
use crate::series::{LastWatched, Series, SeriesParameters};
use anime::remote::{RemoteService, SeriesInfo};
use chrono::{Duration, Utc};
use gumdrop::Options;
use snafu::{ensure, ResultExt};
use std::path::PathBuf;
use std::str;

const ANILIST_CLIENT_ID: u32 = 427;

#[derive(Options)]
pub struct CmdOptions {
    #[options(help = "print help message")]
    help: bool,
    #[options(free, help = "the nickname of the series to watch")]
    pub series: Option<String>,
    #[options(help = "the custom regex pattern to match episode files with")]
    pub matcher: Option<String>,
    #[options(help = "run in offline mode")]
    pub offline: bool,
    #[options(help = "the path to the series")]
    pub path: Option<PathBuf>,
    #[options(help = "play a single episode from the specified or last watched series")]
    pub single: bool,
    #[options(help = "your account access token")]
    pub token: Option<String>,
    #[options(
        no_short,
        help = "fetch series info from AniList for use with offline mode"
    )]
    pub prefetch: bool,
    #[options(no_short, help = "syncronize changes made while offline to AniList")]
    pub sync: bool,
}

fn main() {
    let args = CmdOptions::parse_args_default_or_exit();

    if let Err(err) = run(args) {
        err::display_error(err);
        std::process::exit(1);
    }
}

fn run(args: CmdOptions) -> Result<()> {
    if args.single {
        play_episode(args)
    } else if args.prefetch {
        prefetch(args)
    } else if args.sync {
        sync(args)
    } else {
        tui::run(args)
    }
}

fn series_params_from_args(args: &CmdOptions) -> SeriesParameters {
    SeriesParameters {
        id: None, // TODO
        path: args.path.clone(),
        matcher: args.matcher.clone(),
    }
}

fn init_remote(args: &CmdOptions, can_use_offline: bool) -> Result<Box<dyn RemoteService>> {
    use anime::remote::anilist::AniList;
    use anime::remote::offline::Offline;
    use anime::remote::AccessToken;

    if args.offline {
        ensure!(can_use_offline, err::MustRunOnline);
        Ok(Box::new(Offline::new()))
    } else {
        let token = match &args.token {
            Some(token) => {
                let token = AccessToken::encode(token);
                token.save()?;
                token
            }
            None => match AccessToken::load() {
                Ok(token) => token,
                Err(ref err) if err.is_file_nonexistant() => {
                    return Err(err::Error::NeedAniListToken);
                }
                Err(err) => return Err(err),
            },
        };

        let anilist = AniList::authenticated(token)?;
        Ok(Box::new(anilist))
    }
}

fn prefetch(args: CmdOptions) -> Result<()> {
    let desired_series = match &args.series {
        Some(desired_series) => desired_series,
        None => return Err(err::Error::MustSpecifySeriesName),
    };

    let config = Config::load_or_create()?;
    let db = SeriesDatabase::open()?;
    let remote = init_remote(&args, false)?;
    let params = series_params_from_args(&args);

    let series = Series::from_remote(desired_series, params, &config, remote.as_ref())?;
    series.save(&db)?;

    println!(
        "{} was fetched\nyou can now watch this series offline",
        series.info.title.preferred
    );

    db.close()
}

fn sync(args: CmdOptions) -> Result<()> {
    let db = SeriesDatabase::open()?;
    let mut list_entries = series::database::get_series_entries_need_sync(&db)?;

    if list_entries.is_empty() {
        return Ok(());
    }

    let remote = init_remote(&args, false)?;

    for entry in &mut list_entries {
        match SeriesInfo::select_from_db(&db, entry.id()) {
            Ok(info) => println!("{} is being synced..", info.title.preferred),
            Err(err) => eprintln!(
                "warning: failed to get info for anime with ID {}: {}",
                entry.id(),
                err
            ),
        }

        entry.sync_to_remote(remote.as_ref())?;
        entry.insert_into_db(&db, ())?;
    }

    db.close()
}

fn play_episode(args: CmdOptions) -> Result<()> {
    use anime::remote::Status;

    let config = Config::load_or_create()?;
    let db = SeriesDatabase::open()?;
    let mut last_watched = LastWatched::load()?;

    let remote = init_remote(&args, true)?;
    let remote = remote.as_ref();

    let desired_series = args.series.as_ref().or_else(|| last_watched.get());
    let series_names = series::database::get_series_names(&db)?;

    let mut series = match desired_series {
        Some(desired) if series_names.contains(&desired) => Series::load(&db, &config, desired)?,
        Some(desired) => {
            let params = series_params_from_args(&args);
            let series = Series::from_remote(desired, params, &config, remote)?;
            series.save(&db)?;
            series
        }
        None => return Err(err::Error::MustSpecifySeriesName),
    };

    if last_watched.set(&series.config.nickname) {
        last_watched.save()?;
    }

    series.begin_watching(remote, &config, &db)?;

    let progress_time = {
        let secs_must_watch =
            (series.info.episode_length as f32 * config.episode.pcnt_must_watch) * 60.0;
        let time_must_watch = Duration::seconds(secs_must_watch as i64);

        Utc::now() + time_must_watch
    };

    let next_episode_num = series.entry.watched_eps() + 1;

    let status = series
        .play_episode_cmd(next_episode_num, &config)?
        .status()
        .context(err::FailedToPlayEpisode {
            episode: next_episode_num,
        })?;

    ensure!(status.success(), err::AbnormalPlayerExit);

    if Utc::now() >= progress_time {
        series.episode_completed(remote, &config, &db)?;

        if series.entry.status() == Status::Completed {
            println!("{} completed!", series.info.title.preferred);
        } else {
            println!(
                "{}/{} of {} completed",
                series.entry.watched_eps(),
                series.info.episodes,
                series.info.title.preferred
            );
        }
    } else {
        println!("did not watch long enough to count episode as completed");
    }

    db.close()
}
