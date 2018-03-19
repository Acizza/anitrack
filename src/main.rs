#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde_derive;

extern crate base64;
extern crate chrono;
extern crate mal;
extern crate regex;
extern crate serde;
extern crate serde_json;
extern crate toml;

mod config;
mod error;
mod input;
mod process;
mod series;

use config::Config;
use error::Error;
use mal::MAL;
use series::Series;
use std::path::Path;
use std::path::PathBuf;

fn main() {
    match run() {
        Ok(_) => (),
        Err(e) => {
            let e: failure::Error = e.into();
            eprintln!("fatal error: {}", e.cause());

            for cause in e.causes().skip(1) {
                eprintln!("cause: {}", cause);
            }

            eprintln!("{}", e.backtrace());
        }
    }
}

fn run() -> Result<(), Error> {
    let matches = clap_app!(anitrack =>
        (version: env!("CARGO_PKG_VERSION"))
        (author: env!("CARGO_PKG_AUTHORS"))
        (@arg NAME: "The name of the series to watch")
        (@arg PATH: -p --path +takes_value "Specifies the directory to look for video files in")
        (@arg CONFIG_PATH: -c --config "Specifies the location of the configuration file")
        (@arg SEASON: -s --season +takes_value "Specifies which season you want to watch")
        (@arg DONT_SAVE_PASS: --dontsavepass "Disables saving of your account password")
    ).get_matches();

    let mut config = load_config(&matches)?;
    let path = get_series_path(&mut config, &matches)?;
    let mal = init_mal_client(&matches, &mut config)?;

    config.save(!matches.is_present("DONT_SAVE_PASS"))?;

    let season = matches
        .value_of("SEASON")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let mut series = Series::from_dir(&path, &mal)?;
    series.load_season(season)?.play_all_episodes()?;

    Ok(())
}

fn load_config(args: &clap::ArgMatches) -> Result<Config, Error> {
    let path = args.value_of("CONFIG_PATH").map(Path::new);
    let mut config = config::load(path)?;

    config.remove_invalid_series();

    Ok(config)
}

fn get_series_path(config: &mut Config, args: &clap::ArgMatches) -> Result<PathBuf, Error> {
    match args.value_of("PATH") {
        Some(path) => {
            if let Some(series_name) = args.value_of("NAME") {
                config.series.insert(series_name.into(), path.into());
            }

            Ok(path.into())
        }
        None => match args.value_of("NAME") {
            Some(series_name) => {
                let found = config.series.iter().find(|&(name, _)| name == series_name);

                match found {
                    Some((_, path)) => Ok(path.into()),
                    None => Err(Error::SeriesNotFound(series_name.into())),
                }
            }
            None => Err(Error::NoSeriesInfoProvided),
        },
    }
}

fn init_mal_client<'a>(args: &clap::ArgMatches, config: &mut Config) -> Result<MAL<'a>, Error> {
    let mut mal = {
        let decoded_password = config.user.decode_password()?;
        MAL::new(config.user.name.clone(), decoded_password)
    };

    let mut password_changed = false;

    while !mal.verify_credentials()? {
        println!(
            "invalid password for [{}], please try again:",
            config.user.name
        );

        mal.password = input::read_line()?;
        password_changed = true;
    }

    if !args.is_present("DONT_SAVE_CONFIG") && password_changed {
        config.user.encode_password(&mal.password);
    }

    Ok(mal)
}