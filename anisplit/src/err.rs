use snafu::{Backtrace, ErrorCompat, GenerateBacktrace, Snafu};
use std::io;
use std::path;
use std::result;

pub type Result<T> = result::Result<T, Error>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("anime lib error: {}", source))]
    Anime {
        source: anime::Error,
        backtrace: Backtrace,
    },

    #[snafu(display("io error: {}", source))]
    IO {
        source: io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display("file io error [{:?}]: {}", path, source))]
    FileIO {
        path: path::PathBuf,
        source: io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "link creation failed\n\tfrom: {:?}\n\tto: {:?}\nreason: {}",
        from,
        to,
        source
    ))]
    LinkIO {
        from: path::PathBuf,
        to: path::PathBuf,
        source: io::Error,
    },

    #[snafu(display("path must be a directory"))]
    NotADirectory,

    #[snafu(display("failed to parse series ID: {}", source))]
    InvalidSeriesID { source: std::num::ParseIntError },

    #[snafu(display("missing group \"{}\" in name format", group))]
    MissingFormatGroup { group: String },

    #[snafu(display("directory has no name"))]
    NoDirName,

    #[snafu(display("specified path has no parent directory"))]
    NoDirParent,

    #[snafu(display("failed to parse title from specified path: {}", path))]
    TitleParseFailed { path: String },

    #[snafu(display(
        "unable to find matching series with name: {}\nconsider supplying the ID instead with -i",
        title
    ))]
    UnableToDetectSeries { title: String },
}

impl From<anime::Error> for Error {
    fn from(source: anime::Error) -> Error {
        Error::Anime {
            source,
            backtrace: Backtrace::generate(),
        }
    }
}

pub fn display_error(err: Error) {
    eprintln!("{}", err);

    if let Some(backtrace) = err.backtrace() {
        eprintln!("backtrace:\n{}", backtrace);
    }
}