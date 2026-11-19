use std::{
    collections::HashMap, env, error::Error, fs, io, path::Path, sync::OnceLock, time::Duration,
};

use regex::Regex;
use serde::Deserialize;

use super::{CLIENT_TIMEOUT, DEFAULT_CACHE_DURATION, DEFAULT_TIMEOUT};

pub(super) struct MonitorConfig {
    pub cache: Duration,
    pub sites: Vec<Site>,
}

pub(super) struct Site {
    pub title: String,
    pub check_url: String,
    pub timeout: Duration,
    pub allow_insecure: bool,
    pub alt_status_codes: Vec<u16>,
    pub basic_auth: Option<BasicAuth>,
}

#[derive(Deserialize)]
pub(super) struct BasicAuth {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ConfigFile {
    Standalone(MonitorSettings),
    Glance { pages: Vec<Page> },
    Pages(Vec<Page>),
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Page {
    #[serde(default)]
    head_widgets: Vec<Widget>,
    #[serde(default)]
    columns: Vec<Column>,
}

#[derive(Deserialize)]
struct Column {
    widgets: Vec<Widget>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Widget {
    Monitor(MonitorSettings),
    #[serde(alias = "split-column")]
    Group {
        widgets: Vec<Widget>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MonitorSettings {
    cache: Option<String>,
    sites: Vec<SiteSettings>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SiteSettings {
    title: String,
    #[serde(default)]
    url: String,
    check_url: Option<String>,
    timeout: Option<String>,
    #[serde(default)]
    allow_insecure: bool,
    #[serde(default)]
    alt_status_codes: Vec<u16>,
    basic_auth: Option<BasicAuth>,
}

pub(super) fn load(
    path: &Path,
    env_file: Option<&Path>,
) -> Result<Vec<MonitorConfig>, Box<dyn Error>> {
    let file_env = match env_file {
        Some(path) => dotenvy::from_path_iter(path)?.collect::<Result<HashMap<_, _>, _>>()?,
        None => HashMap::new(),
    };
    let contents = fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("reading monitor settings from {}: {error}", path.display()),
        )
    })?;
    parse(&contents, |name| {
        env::var(name).ok().or_else(|| file_env.get(name).cloned())
    })
    .map_err(|error| format!("monitor settings in {}: {error}", path.display()).into())
}

pub(super) fn parse(
    contents: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Vec<MonitorConfig>, Box<dyn Error>> {
    let mut monitors = Vec::new();
    match serde_saphyr::from_str::<ConfigFile>(contents)? {
        ConfigFile::Standalone(monitor) => monitors.push(monitor),
        ConfigFile::Glance { pages } | ConfigFile::Pages(pages) => {
            for page in pages {
                collect_monitors(page.head_widgets, &mut monitors);
                for column in page.columns {
                    collect_monitors(column.widgets, &mut monitors);
                }
            }
        }
    }
    if monitors.is_empty() {
        return Err("no Monitor widgets found".into());
    }

    monitors
        .into_iter()
        .map(|monitor| {
            Ok(MonitorConfig {
                cache: duration(
                    monitor.cache.as_deref(),
                    "cache",
                    DEFAULT_CACHE_DURATION,
                    &lookup,
                )?,
                sites: monitor
                    .sites
                    .into_iter()
                    .map(|site| site.resolve(&lookup))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect()
}

fn collect_monitors(widgets: Vec<Widget>, monitors: &mut Vec<MonitorSettings>) {
    for widget in widgets {
        match widget {
            Widget::Monitor(monitor) => monitors.push(monitor),
            Widget::Group { widgets } => collect_monitors(widgets, monitors),
            Widget::Other => {}
        }
    }
}

impl SiteSettings {
    fn resolve(self, lookup: &impl Fn(&str) -> Option<String>) -> Result<Site, Box<dyn Error>> {
        let title = expand(&self.title, lookup)?;
        let check_url = match self.check_url {
            Some(url) => expand(&url, lookup)?,
            None => String::new(),
        };
        let check_url = if check_url.is_empty() {
            expand(&self.url, lookup)?
        } else {
            check_url
        };
        let basic_auth = self
            .basic_auth
            .map(|auth| -> Result<_, Box<dyn Error>> {
                Ok(BasicAuth {
                    username: expand(&auth.username, lookup)?,
                    password: expand(&auth.password, lookup)?,
                })
            })
            .transpose()?
            .filter(|auth| !auth.username.is_empty() || !auth.password.is_empty());

        Ok(Site {
            title,
            check_url,
            timeout: duration(self.timeout.as_deref(), "timeout", DEFAULT_TIMEOUT, lookup)?
                .min(CLIENT_TIMEOUT),
            allow_insecure: self.allow_insecure,
            alt_status_codes: self.alt_status_codes,
            basic_auth,
        })
    }
}

fn duration(
    value: Option<&str>,
    field: &str,
    default: Duration,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Duration, Box<dyn Error>> {
    let Some(value) = value else {
        return Ok(default);
    };
    let value = expand(value, lookup)?;
    let invalid = || format!("invalid {field}, expected a number followed by s, m, h, or d");
    let seconds_per_unit = match value.as_bytes().last() {
        Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 60 * 60,
        Some(b'd') => 24 * 60 * 60,
        _ => return Err(invalid().into()),
    };
    let count = &value[..value.len() - 1];
    if !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid().into());
    }
    let seconds = count
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(seconds_per_unit))
        .filter(|seconds| *seconds <= i64::MAX as u64 / 1_000_000_000)
        .ok_or_else(invalid)?;
    if seconds == 0 {
        return Ok(default);
    }
    Ok(Duration::from_secs(seconds))
}

fn expand(value: &str, lookup: &impl Fn(&str) -> Option<String>) -> Result<String, Box<dyn Error>> {
    static VARIABLE: OnceLock<Regex> = OnceLock::new();
    let pattern = VARIABLE.get_or_init(|| Regex::new(r"(\\)?\$\{([A-Z0-9_]+)\}").unwrap());
    let mut result = String::new();
    let mut previous = 0;
    for captures in pattern.captures_iter(value) {
        let whole = captures.get(0).unwrap();
        result.push_str(&value[previous..whole.start()]);
        if captures.get(1).is_some() {
            result.push_str(&whole.as_str()[1..]);
        } else {
            let name = &captures[2];
            let replacement =
                lookup(name).ok_or_else(|| format!("environment variable {name} not found"))?;
            result.push_str(&replacement);
        }
        previous = whole.end();
    }
    result.push_str(&value[previous..]);
    Ok(result)
}
