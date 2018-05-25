use config::Config;
use error::BackendError;

pub mod anilist;

pub trait SyncBackend
where
    Self: Sized,
{
    fn init(config: &mut Config) -> Result<Self, BackendError>;
    fn find_series_by_name(&self, name: &str) -> Result<Vec<AnimeInfo>, BackendError>;
}

#[derive(Debug, Deserialize)]
pub struct AnimeInfo {
    pub id: u32,
    pub title: String,
    pub episodes: u32,
}
