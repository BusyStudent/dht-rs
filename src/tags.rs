#![allow(dead_code)]

use super::storage::TorrentFile;
use std::{collections::{HashMap, HashSet}, fmt, path::Path};

#[derive(Debug, PartialEq, Eq, Hash, Copy, Clone)]
enum Category {
    Video,
    Music,
    Book,
    Game,
    Picture,  // 
    Software, // 
    Adult,    // Porn, XXX
    Archive,  // Zip, Rar, etc
}

#[derive(Debug)]
struct ExtensionRule {
    extension: &'static str,
    category: Category,
    base_score: f64,
}

/// Rules apply on the filename
#[derive(Debug)]
struct ContentRule {
    keyword: &'static str,
    category: Category,
    score_bonus: f64,
}

#[derive(Debug)]
struct MetadataRule {
    keywords: &'static [&'static str],
    tag: &'static str,
}

const EXTENSION_RULES: &[ExtensionRule] = &[
    // Video
    ExtensionRule { extension: "mkv", category: Category::Video, base_score: 100.0 },
    ExtensionRule { extension: "mp4", category: Category::Video, base_score: 95.0 }, 
    ExtensionRule { extension: "avi", category: Category::Video, base_score: 90.0 },
    ExtensionRule { extension: "ts", category: Category::Video, base_score: 100.0 },
    ExtensionRule { extension: "mov", category: Category::Video, base_score: 90.0 },
    ExtensionRule { extension: "wmv", category: Category::Video, base_score: 90.0 },

    // Music
    ExtensionRule { extension: "mp3", category: Category::Music, base_score: 100.0 },
    ExtensionRule { extension: "flac", category: Category::Music, base_score: 100.0 },
    ExtensionRule { extension: "wav", category: Category::Music, base_score: 100.0 },
    ExtensionRule { extension: "ape", category: Category::Music, base_score: 100.0 },
    ExtensionRule { extension: "ogg", category: Category::Music, base_score: 100.0 },
    ExtensionRule { extension: "m4a", category: Category::Music, base_score: 100.0 },
    
    // Software/Game
    ExtensionRule { extension: "iso", category: Category::Software, base_score: 100.0 },
    ExtensionRule { extension: "exe", category: Category::Software, base_score: 80.0 },
    ExtensionRule { extension: "apk", category: Category::Software, base_score: 80.0 },

    // Book
    ExtensionRule { extension: "pdf", category: Category::Book, base_score: 100.0 },
    ExtensionRule { extension: "epub", category: Category::Book, base_score: 100.0 },
    ExtensionRule { extension: "mobi", category: Category::Book, base_score: 100.0 },

    // Picture
    ExtensionRule { extension: "jpg", category: Category::Picture, base_score: 20.0 }, // We may find picture in another category, so it has a low score
    ExtensionRule { extension: "png", category: Category::Picture, base_score: 20.0 },
    ExtensionRule { extension: "gif", category: Category::Picture, base_score: 20.0 },
    

    // Archive
    ExtensionRule { extension: "zip", category: Category::Archive, base_score: 10.0 },
    ExtensionRule { extension: "rar", category: Category::Archive, base_score: 10.0 },
];

const CONTENT_RULES: &[ContentRule] = &[
    ContentRule { keyword: "episode", category: Category::Video, score_bonus: 50.0 },
    ContentRule { keyword: "season", category: Category::Video, score_bonus: 50.0 },
    ContentRule { keyword: "s01e01", category: Category::Video, score_bonus: 60.0 }, // TODO: Use regex
    ContentRule { keyword: "soundtrack", category: Category::Music, score_bonus: 50.0 },
    ContentRule { keyword: "ost", category: Category::Music, score_bonus: 50.0 },
    ContentRule { keyword: "album", category: Category::Music, score_bonus: 40.0 },

    ContentRule { keyword: "fitgirl", category: Category::Game, score_bonus: 500.0 }, // FitGirl is a game crack group, so it's a game
    ContentRule { keyword: "repack", category: Category::Game, score_bonus: 50.0 },

    ContentRule { keyword: "同人誌", category: Category::Book, score_bonus: 50.0 }, // Book?

    ContentRule { keyword: "sex", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "ntr", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "porn", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "無修正", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "无修正", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "decensored", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "hentai", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "18+", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "屌", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "内射", category: Category::Adult, score_bonus: 500.0 },
    ContentRule { keyword: "自慰", category: Category::Adult, score_bonus: 500.0 },
];

const METADATA_RULES: &[MetadataRule] = &[
    MetadataRule { keywords: &["1080p", "1080i"], tag: "1080p" },
    MetadataRule { keywords: &["720p", "720i"], tag: "720p" },
    MetadataRule { keywords: &["2160p", "4k", "uhd"], tag: "4k" },
    MetadataRule { keywords: &["bluray", "blu-ray", "bdrip"], tag: "bluray" },
    MetadataRule { keywords: &["web-dl", "webrip", "dl版"], tag: "web" },
    MetadataRule { keywords: &["hdtv"], tag: "hdtv" },
    MetadataRule { keywords: &["x264", "h264", "avc"], tag: "x264" },
    MetadataRule { keywords: &["x265", "h265", "hevc"], tag: "x265" },
    MetadataRule { keywords: &["aac"], tag: "aac" },
    MetadataRule { keywords: &["dts"], tag: "dts" },
    MetadataRule { keywords: &["flac"], tag: "flac" },
    MetadataRule { keywords: &["mp3"], tag: "mp3" },
    MetadataRule { keywords: &["cht", "繁體"], tag: "cht" },
    MetadataRule { keywords: &["chs", "简体", "汉化", "中文", "chinese"], tag: "chs" },

    // For Anime
    MetadataRule { keywords: ANIME_KEYWORDS, tag: "anime" },
];

/// The category can be duplicated like (Game, Adult)
const SPECIAL_CATEGORIES: &[Category] = &[
    Category::Adult,
];

/// Common keywords for anime
const ANIME_KEYWORDS: &[&'static str] = &[
  "nekomoe",
  "uha-wings",
  "airota",
  "mabors",
  "xksub",
  "dmg",
  "sakurasub",
  "sweetsub",
  "lolihouse",
  "vcb-studio",
  "philo-raws",
  "wanko",
  "sumisora",
  "kamigami",
  "gensho",
  "ktxp",
  "flsnow",
  "fysub",
  "lilith-raws",
  "moozzi2",
  "nc-raws",
  "ohys-raws",
  "baha",
  "b-global",
  "bilibili",
  "netflix",
  "crunchyroll",
  "桜都字幕组"
];

/// Generates a list of tags from a file name, return tags, score
pub fn generate_torrent_tags(name: &str, files: &[TorrentFile]) -> Vec<String> {
    let total_size = files.iter().map(|f| f.size).sum::<u64>().max(1);
    let mut categories = HashMap::new();
    let mut combined_name = String::from(name); // All file names combined
    let mut set = HashSet::new();

    for file in files {
        let path = Path::new(&file.name);
        let weight = file.size as f64 / total_size as f64;
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            combined_name.push_str(&name.to_lowercase());
            combined_name.push(' ');
        }
        let extension = match path.extension().and_then(|s| s.to_str()) {
            Some(val) => val,
            None => continue,
        };
        for rule in EXTENSION_RULES {
            if !rule.extension.eq_ignore_ascii_case(extension) {
                continue;
            }
            let score = categories.entry(rule.category).or_insert(0.0);
            *score += rule.base_score * weight;
            break;
        }
    }

    for rule in CONTENT_RULES {
        if !combined_name.contains(rule.keyword) {
            continue;
        }
        let score = categories.entry(rule.category).or_insert(0.0);
        *score += rule.score_bonus;
    }

    // Collect possible categories
    if let Some(category) = categories.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()) {
        set.insert(category.0.to_string());
    }

    for category in SPECIAL_CATEGORIES {
        if let Some(score) = categories.get(category) {
            if *score > 100.0 {
                set.insert(category.to_string());
            }
        }
    }

    // Add metadata tags
    for rule in METADATA_RULES {
        for keyword in rule.keywords {
            if combined_name.contains(keyword) {
                set.insert(rule.tag.into());
                break;
            }
        }
    }

    return set.into_iter().collect();
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        return match *self {
            Self::Adult => write!(f, "adult"),
            Self::Video => write!(f, "video"),
            Self::Music => write!(f, "music"),
            Self::Book => write!(f, "book"),
            Self::Picture => write!(f, "picture"),
            Self::Game => write!(f, "game"),
            Self::Software => write!(f, "software"),
            Self::Archive => write!(f, "archive"),
        };
    }
}