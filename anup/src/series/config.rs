use super::{SeriesParams, SeriesPath, UpdateParams};
use crate::database::schema::series_configs;
use crate::database::{self, Database};
use crate::err::{Error, Result};
use anime::local::EpisodeParser;
use diesel::prelude::*;
use std::borrow::Cow;

#[derive(Clone, Debug, Queryable, Insertable)]
pub struct SeriesConfig {
    pub id: i32,
    pub nickname: String,
    pub path: SeriesPath,
    pub episode_parser: EpisodeParser,
    pub player_args: database::PlayerArgs,
}

impl SeriesConfig {
    pub fn new(id: i32, params: SeriesParams, db: &Database) -> Result<Self> {
        if let Some(existing) = Self::exists(db, id, &params) {
            return Err(Error::SeriesAlreadyExists { name: existing });
        }

        Ok(Self {
            id,
            nickname: params.name,
            path: params.path,
            episode_parser: params.parser,
            player_args: database::PlayerArgs::new(),
        })
    }

    pub fn update(&mut self, params: UpdateParams, db: &Database) -> Result<()> {
        if let Some(id) = params.id {
            if let Some(existing) = Self::id_exists(db, id) {
                return Err(Error::SeriesAlreadyExists { name: existing });
            }

            self.id = id;
        }

        if let Some(path) = params.path {
            self.path = path;
        }

        if let Some(parser) = params.parser {
            self.episode_parser = parser;
        }

        Ok(())
    }

    pub fn save(&self, db: &Database) -> diesel::QueryResult<usize> {
        use crate::database::schema::series_configs::dsl::*;

        diesel::replace_into(series_configs)
            .values(self)
            .execute(db.conn())
    }

    pub fn load_all(db: &Database) -> diesel::QueryResult<Vec<Self>> {
        use crate::database::schema::series_configs::dsl::*;

        series_configs.load(db.conn())
    }

    pub fn load_by_name<S>(db: &Database, name: S) -> diesel::QueryResult<Self>
    where
        S: AsRef<str>,
    {
        use crate::database::schema::series_configs::dsl::*;

        series_configs
            .filter(nickname.eq(name.as_ref()))
            .get_result(db.conn())
    }

    /// Delete the series configuration from the database.
    ///
    /// This will also remove the series info and entry, if it exists.
    pub fn delete(&self, db: &Database) -> diesel::QueryResult<usize> {
        use crate::database::schema::series_configs::dsl::*;

        diesel::delete(series_configs.filter(id.eq(self.id))).execute(db.conn())
    }

    pub fn exists(db: &Database, config_id: i32, params: &SeriesParams) -> Option<String> {
        use crate::database::schema::series_configs::dsl::*;

        series_configs
            .filter(id.eq(config_id).or(nickname.eq(&params.name)))
            .select(nickname)
            .get_result(db.conn())
            .ok()
    }

    fn id_exists(db: &Database, config_id: i32) -> Option<String> {
        use crate::database::schema::series_configs::dsl::*;

        series_configs
            .filter(id.eq(config_id))
            .select(nickname)
            .get_result(db.conn())
            .ok()
    }
}

impl PartialEq<String> for SeriesConfig {
    fn eq(&self, nickname: &String) -> bool {
        self.nickname == *nickname
    }
}

impl<'a> Into<Cow<'a, Self>> for SeriesConfig {
    fn into(self) -> Cow<'a, Self> {
        Cow::Owned(self)
    }
}

impl<'a> Into<Cow<'a, SeriesConfig>> for &'a SeriesConfig {
    fn into(self) -> Cow<'a, SeriesConfig> {
        Cow::Borrowed(self)
    }
}
