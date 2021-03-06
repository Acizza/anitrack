use super::SeriesPath;
use crate::database::schema::series_info;
use crate::database::Database;
use anime::remote::{Remote, RemoteService, SeriesID, SeriesInfo as RemoteInfo};
use anyhow::Result;
use diesel::prelude::*;
use std::borrow::Cow;

#[derive(Clone, Queryable, Insertable)]
#[table_name = "series_info"]
pub struct SeriesInfo {
    pub id: i32,
    pub title_preferred: String,
    pub title_romaji: String,
    pub episodes: i16,
    pub episode_length_mins: i16,
}

impl SeriesInfo {
    pub fn load(db: &Database, info_id: i32) -> diesel::QueryResult<Self> {
        use crate::database::schema::series_info::dsl::{id, series_info};

        series_info.filter(id.eq(info_id)).get_result(db.conn())
    }

    pub fn save(&self, db: &Database) -> diesel::QueryResult<usize> {
        use crate::database::schema::series_info::dsl::series_info;

        diesel::replace_into(series_info)
            .values(self)
            .execute(db.conn())
    }

    pub fn from_remote(sel: InfoSelector, remote: &Remote) -> Result<InfoResult> {
        match sel {
            InfoSelector::ID(id) => Self::from_remote_by_id(id, remote).map(InfoResult::Confident),
            InfoSelector::Name(name) => Self::from_remote_by_name(name, remote),
        }
    }

    pub fn from_remote_by_id(id: SeriesID, remote: &Remote) -> Result<Self> {
        remote
            .search_info_by_id(id)
            .map(Into::into)
            .map_err(Into::into)
    }

    pub fn from_remote_by_name<S>(name: S, remote: &Remote) -> Result<InfoResult>
    where
        S: Into<String>,
    {
        const MIN_CONFIDENCE: f32 = 0.85;

        let name = name.into();
        let mut results = remote.search_info_by_name(&name)?;
        let found =
            RemoteInfo::closest_match(name, MIN_CONFIDENCE, results.iter().map(Cow::Borrowed));

        match found {
            Some((best_match, _)) => {
                let info = results.swap_remove(best_match).into();
                Ok(InfoResult::Confident(info))
            }
            None => Ok(InfoResult::Unconfident(
                results.into_iter().map(Into::into).collect(),
            )),
        }
    }
}

impl From<anime::remote::SeriesInfo> for SeriesInfo {
    #[allow(clippy::cast_possible_wrap)]
    fn from(value: anime::remote::SeriesInfo) -> Self {
        Self {
            id: value.id as i32,
            title_preferred: value.title.preferred,
            title_romaji: value.title.romaji,
            episodes: value.episodes as i16,
            episode_length_mins: value.episode_length as i16,
        }
    }
}

#[allow(variant_size_differences)]
pub enum InfoSelector {
    Name(String),
    ID(SeriesID),
}

impl InfoSelector {
    pub fn from_path_or_name<'a, P, S>(path: P, name: S) -> Self
    where
        P: Into<Cow<'a, SeriesPath>>,
        S: Into<String>,
    {
        use anime::local::detect::dir;
        let path = path.into();
        dir::parse_title(path.inner()).map_or_else(|| Self::Name(name.into()), Self::Name)
    }
}

pub enum InfoResult {
    Confident(SeriesInfo),
    Unconfident(Vec<SeriesInfo>),
}
