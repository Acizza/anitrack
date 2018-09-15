#![feature(nll)]

#[cfg(windows)]
extern crate winapi;

#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;

extern crate base64;
extern crate chrono;
extern crate directories;
extern crate regex;
extern crate reqwest;
extern crate serde;
extern crate toml;

mod backend;
mod config;
mod error;
mod input;
mod process;
mod series;
mod util;

use backend::{anilist::AniList, SyncBackend};
use config::Config;
use error::{Error, SeriesError};
use series::dir::{FolderData, SaveData, SeriesInfo};
use series::{Series, SeriesConfig};
use std::path::PathBuf;

fn main() {
    match run() {
        Ok(_) => (),
        Err(Error::Series(SeriesError::RequestExit)) => (),
        Err(e) => {
            let e: failure::Error = e.into();
            eprintln!("fatal error: {}", e);

            for cause in e.iter_chain().skip(1) {
                eprintln!("cause: {}", cause);
            }

            eprintln!("{}", e.backtrace());
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Error> {
    let args = clap_app!(anup =>
        (version: env!("CARGO_PKG_VERSION"))
        (author: env!("CARGO_PKG_AUTHORS"))
        (@arg NAME: "The name of the series to watch")
        (@arg PATH: -p --path +takes_value "Specifies the directory to look for video files in")
        (@arg SEASON: -s --season +takes_value "Specifies which season you want to watch")
        (@arg INFO: -i --info "Displays saved series information")
        (@arg EDIT: -e --edit "Displays options for the series instead of playing it")
        (@arg OFFLINE: -o --offline "Launches the program in offline mode")
        (@arg SYNC: --sync "Synchronizes all changes made offline to AniList")
    ).get_matches();

    if args.is_present("INFO") {
        print_saved_series_info()
    } else if args.is_present("SYNC") {
        sync_offline_changes()
    } else {
        watch_series(&args)
    }
}

fn watch_series(args: &clap::ArgMatches) -> Result<(), Error> {
    let mut config = Config::load()?;
    config.remove_invalid_series();

    let path = get_series_path(&mut config, args)?;
    let offline_mode = args.is_present("OFFLINE");

    let sync_backend = AniList::init(offline_mode, &mut config)?;

    config.save()?;

    let season = {
        let value: usize = args
            .value_of("SEASON")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        value.saturating_sub(1)
    };

    let config = SeriesConfig {
        offline_mode,
        sync_service: sync_backend,
        season_num: season,
    };

    let folder_data = FolderData::load_dir(&path)?;

    let mut series = Series::init(config, folder_data)?;
    series.sync_remote_states()?;

    if args.is_present("EDIT") {
        series.prompt_series_options()?;
        return Ok(());
    }

    series.prepare_initial_state()?;
    series.play_all_episodes()?;

    Ok(())
}

fn print_saved_series_info() -> Result<(), Error> {
    let mut config = Config::load()?;
    config.remove_invalid_series();

    println!("found [{}] series:\n", config.series.len());

    for (name, path) in config.series {
        let savefile = match SaveData::from_dir(&path) {
            Ok(data) => data,
            Err(_) => continue,
        };

        let ep_data = series::dir::parse_episode_files(&path, savefile.episode_matcher.clone());

        let ep_list = match ep_data {
            Ok(mut ep_data) => {
                let series_info = SeriesInfo::select_from_save(&mut ep_data, &savefile);

                match series_info {
                    Some(info) => {
                        let mut ep_nums = info.episodes.keys().cloned().collect::<Vec<_>>();
                        ep_nums.sort_unstable();

                        util::concat_sequential_values(&ep_nums, "..", " | ")
                            .unwrap_or_else(|| "none".into())
                    }
                    None => "needs series info".into(),
                }
            }
            Err(SeriesError::NoSeriesFound) => "none".into(),
            Err(err) => {
                eprintln!("failed to parse episode data for [{}]: {}", name, err);
                continue;
            }
        };

        println!("[{}]:", name);
        println!(
            "  path: {}\n  episodes on disk: {}",
            path.to_string_lossy(),
            ep_list
        );
    }

    Ok(())
}

fn sync_offline_changes() -> Result<(), Error> {
    let mut config = Config::load()?;
    config.remove_invalid_series();

    let backend = AniList::init(false, &mut config)?;

    for (_, path) in config.series {
        let mut save_data = match SaveData::from_dir(&path) {
            Ok(save_data) => save_data,
            Err(_) => continue,
        };

        for season_num in 0..save_data.season_states.len() {
            let season_state = &mut save_data.season_states[season_num];

            if !season_state.needs_sync {
                continue;
            }

            println!("[{}] syncing..", season_state.state.info.title);

            if season_state.needs_info {
                season_state.state.info = series::search_for_series_info(
                    &backend,
                    &season_state.state.info.title,
                    season_num,
                )?;

                season_state.needs_info = false;
            }

            backend.update_list_entry(&season_state.state)?;
            season_state.needs_sync = false;
        }

        save_data.write_to_file()?;
    }

    Ok(())
}

fn get_series_path(config: &mut Config, args: &clap::ArgMatches) -> Result<PathBuf, Error> {
    match args.value_of("PATH") {
        Some(path) => {
            if let Some(series_name) = args.value_of("NAME") {
                config.series.insert(series_name.into(), path.into());
            }

            Ok(path.into())
        }
        None => {
            let name = args.value_of("NAME").ok_or(Error::NoSeriesInfoProvided)?;

            config
                .series
                .get(name)
                .ok_or_else(|| Error::SeriesNotFound(name.into()))
                .map(|path| path.into())
        }
    }
}
