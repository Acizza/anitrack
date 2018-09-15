use super::{SeasonState, SeriesConfig};
use backend::{AnimeEntry, AnimeInfo, SyncBackend};
use error::SeriesError;
use input::{self, Answer};
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use toml;

pub struct FolderData {
    pub series_info: SeriesInfo,
    pub savefile: SaveData,
    pub path: PathBuf,
}

impl FolderData {
    pub fn load_dir(path: &Path) -> Result<FolderData, SeriesError> {
        let mut savefile = SaveData::from_dir(path)?;
        let series_info = FolderData::load_series_info(path, &mut savefile)?;

        Ok(FolderData {
            series_info,
            savefile,
            path: PathBuf::from(path),
        })
    }

    fn load_series_info(path: &Path, savefile: &mut SaveData) -> Result<SeriesInfo, SeriesError> {
        let mut ep_data = parse_episode_files_until_valid(path, &mut savefile.episode_matcher)?;

        if let Some(info) = SeriesInfo::select_from_save(&mut ep_data, savefile) {
            return Ok(info);
        }

        let info = prompt_select_series_info(ep_data)?;
        savefile.files_title = Some(info.name.clone());

        Ok(info)
    }

    pub fn populate_season_data<B>(&mut self, config: &SeriesConfig<B>) -> Result<(), SeriesError>
    where
        B: SyncBackend,
    {
        let num_seasons = self.seasons().len();

        if num_seasons > config.season_num {
            return Ok(());
        }

        for cur_season in num_seasons..=config.season_num {
            let info = AnimeInfo::default();
            let entry = AnimeEntry::new(info);

            let mut season = SeasonState {
                state: entry,
                needs_info: true,
                needs_sync: config.offline_mode,
            };

            season.sync_info_from_remote(config, &self, cur_season)?;

            self.seasons_mut().push(season);
        }

        Ok(())
    }

    pub fn calculate_season_offset(&self, mut range: Range<usize>) -> u32 {
        let num_seasons = self.savefile.season_states.len();
        range.start = num_seasons.min(range.start);
        range.end = num_seasons.min(range.end);

        let mut offset = 0;

        for i in range {
            let season = &self.savefile.season_states[i];

            match season.state.info.episodes {
                Some(eps) => offset += eps,
                None => return offset,
            }
        }

        offset
    }

    pub fn try_remove_dir(&self) {
        let path = self.path.to_string_lossy();

        println!("WARNING: {} will be deleted", path);
        println!("is this ok? (y/N)");

        match input::read_yn(Answer::No) {
            Ok(true) => match fs::remove_dir_all(&self.path) {
                Ok(_) => (),
                Err(err) => {
                    eprintln!("failed to remove directory: {}", err);
                }
            },
            Ok(false) => (),
            Err(err) => {
                eprintln!("failed to read input: {}", err);
            }
        }
    }

    pub fn get_episode(&self, episode: u32) -> Result<&PathBuf, SeriesError> {
        self.series_info.get_episode(episode)
    }

    pub fn save(&self) -> Result<(), SeriesError> {
        self.savefile.write_to_file()
    }

    pub fn seasons(&self) -> &Vec<SeasonState> {
        &self.savefile.season_states
    }

    pub fn seasons_mut(&mut self) -> &mut Vec<SeasonState> {
        &mut self.savefile.season_states
    }
}

#[derive(Serialize, Deserialize)]
pub struct SaveData {
    pub episode_matcher: Option<String>,
    pub files_title: Option<String>,
    pub season_states: Vec<SeasonState>,
    #[serde(skip)]
    pub path: PathBuf,
}

impl SaveData {
    const DATA_FILE_NAME: &'static str = ".anup";

    pub fn new(path: PathBuf) -> SaveData {
        SaveData {
            episode_matcher: None,
            files_title: None,
            season_states: Vec::new(),
            path,
        }
    }

    pub fn from_dir(path: &Path) -> Result<SaveData, SeriesError> {
        let path = PathBuf::from(path).join(SaveData::DATA_FILE_NAME);

        if !path.exists() {
            return Ok(SaveData::new(path));
        }

        let file_contents = fs::read_to_string(&path)?;

        let mut save_data: SaveData = toml::from_str(&file_contents)?;
        save_data.path = path;

        Ok(save_data)
    }

    pub fn write_to_file(&self) -> Result<(), SeriesError> {
        let toml = toml::to_string(self)?;
        fs::write(&self.path, toml)?;

        Ok(())
    }
}

pub struct SeriesInfo {
    pub name: String,
    pub episodes: HashMap<u32, PathBuf>,
}

impl SeriesInfo {
    pub fn get_episode(&self, episode: u32) -> Result<&PathBuf, SeriesError> {
        self.episodes
            .get(&episode)
            .ok_or_else(|| SeriesError::EpisodeNotFound(episode))
    }

    pub fn select_from_save(
        ep_data: &mut SeriesEpisodes,
        savefile: &SaveData,
    ) -> Option<SeriesInfo> {
        if let Some(name) = &savefile.files_title {
            let entry = ep_data.remove_entry(name);

            if let Some((name, episodes)) = entry {
                return Some(SeriesInfo { name, episodes });
            }
        }

        None
    }
}

struct EpisodeFile {
    series_name: String,
    episode_num: u32,
}

impl EpisodeFile {
    fn parse(path: &Path, matcher: &Regex) -> Result<EpisodeFile, SeriesError> {
        // Replace certain characters with spaces since they can prevent proper series
        // identification or prevent it from being found on a sync backend
        let filename = path
            .file_name()
            .and_then(|path| path.to_str())
            .ok_or(SeriesError::UnableToGetFilename)?;

        let caps = matcher
            .captures(&filename)
            .ok_or(SeriesError::EpisodeRegexCaptureFailed)?;

        let series_name = {
            let raw_name = caps
                .name("name")
                .map(|c| c.as_str())
                .ok_or_else(|| SeriesError::UnknownRegexCapture("name".into()))?;

            raw_name
                .replace('.', " ")
                .replace(" -", ":") // Dashes typically represent a colon in file names
                .replace('_', " ")
                .trim()
                .to_string()
        };

        let episode = caps
            .name("episode")
            .ok_or_else(|| SeriesError::UnknownRegexCapture("episode".into()))
            .and_then(|cap| {
                cap.as_str()
                    .parse()
                    .map_err(SeriesError::EpisodeNumParseFailed)
            })?;

        Ok(EpisodeFile {
            series_name,
            episode_num: episode,
        })
    }
}

type EpisodePaths = HashMap<u32, PathBuf>;
type SeriesEpisodes = HashMap<String, EpisodePaths>;

pub fn prompt_select_series_info(info: SeriesEpisodes) -> Result<SeriesInfo, SeriesError> {
    if info.is_empty() {
        return Err(SeriesError::NoSeriesFound);
    }

    let mut info = info
        .into_iter()
        .map(|(name, eps)| SeriesInfo {
            name,
            episodes: eps,
        }).collect::<Vec<_>>();

    if info.len() == 1 {
        return Ok(info.remove(0));
    }

    println!("multiple series found in directory");
    println!("please enter the number next to the episode files you want to use:");

    for (i, series) in info.iter().enumerate() {
        println!("{} [{}]", 1 + i, series.name);
    }

    let index = input::read_range(1, info.len())? - 1;
    let series = info.remove(index);

    Ok(series)
}

// This default pattern will match episodes in several common formats, such as:
// [Group] Series Name - 01.mkv
// [Group]_Series_Name_-_01.mkv
// [Group].Series.Name.-.01.mkv
// [Group] Series Name - 01 [tag 1][tag 2].mkv
// [Group]_Series_Name_-_01_[tag1][tag2].mkv
// [Group].Series.Name.-.01.[tag1][tag2].mkv
// Series Name - 01.mkv
// Series_Name_-_01.mkv
// Series.Name.-.01.mkv
const EP_FORMAT_REGEX: &str =
    r"(?:\[.+?\](?:\s+|_+|\.+))?(?P<name>.+?)(?:\s*|_*|\.*)-(?:\s*|_*|\.*)(?P<episode>\d+).*?\..+?";

fn format_episode_parser_regex<'a, S>(pattern: Option<S>) -> Result<Cow<'a, Regex>, SeriesError>
where
    S: AsRef<str>,
{
    lazy_static! {
        static ref EP_FORMAT: Regex = Regex::new(EP_FORMAT_REGEX).unwrap();
    }

    match pattern {
        Some(pattern) => {
            let pattern = pattern
                .as_ref()
                .replace("{name}", "(?P<name>.+?)")
                .replace("{episode}", r"(?P<episode>\d+)");

            let regex = Regex::new(&pattern)?;
            Ok(Cow::Owned(regex))
        }
        None => Ok(Cow::Borrowed(&*EP_FORMAT)),
    }
}

pub fn parse_episode_files<S>(
    path: &Path,
    pattern: Option<S>,
) -> Result<SeriesEpisodes, SeriesError>
where
    S: AsRef<str>,
{
    if !path.is_dir() {
        return Err(SeriesError::NotADirectory(path.to_string_lossy().into()));
    }

    let pattern = format_episode_parser_regex(pattern)?;
    let mut data = HashMap::new();

    for entry in fs::read_dir(path).map_err(SeriesError::Io)? {
        let entry = entry.map_err(SeriesError::Io)?.path();

        if !entry.is_file() {
            continue;
        }

        let episode = match EpisodeFile::parse(&entry, pattern.as_ref()) {
            Ok(episode) => episode,
            Err(SeriesError::EpisodeRegexCaptureFailed) => continue,
            Err(err) => return Err(err),
        };

        let series = data.entry(episode.series_name).or_insert_with(HashMap::new);
        series.insert(episode.episode_num, entry);
    }

    if data.is_empty() {
        return Err(SeriesError::NoSeriesFound);
    }

    Ok(data)
}

pub fn parse_episode_files_until_valid<S>(
    path: &Path,
    pattern: &mut Option<S>,
) -> Result<SeriesEpisodes, SeriesError>
where
    S: AsRef<str> + From<String>,
{
    loop {
        match parse_episode_files(path, pattern.as_ref()) {
            Ok(data) => break Ok(data),
            Err(SeriesError::NoSeriesFound) => {
                println!("no series found");
                println!("you will now be prompted to enter a custom regex pattern");
                println!("when entering the pattern, please mark the series name and episode number with {{name}} and {{episode}}, respectively");
                println!("example:");
                println!("  filename: [SubGroup] Series Name - Ep01.mkv");
                println!(r"  pattern: \[.+?\] {{name}} - Ep{{episode}}.mkv");
                println!("please enter your custom pattern:");

                *pattern = Some(input::read_line()?.into());
            }
            Err(err @ SeriesError::Regex(_)) | Err(err @ SeriesError::UnknownRegexCapture(_)) => {
                eprintln!("error parsing regex pattern: {}", err);
                println!("please try again:");

                *pattern = Some(input::read_line()?.into());
            }
            Err(err) => return Err(err),
        }
    }
}