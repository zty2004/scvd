use serde::{Deserialize, Serialize};

/// A course with its video list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Course {
    pub id: String,
    pub name: String,
    pub teacher: String,
    pub subject_name: String,
    pub videos: Vec<VideoInfo>,
}

/// Single video metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub name: String,
    pub url: String,
    /// View count / playback order index
    pub view_num: i64,
    /// True if this is the main recording (not a supplementary screencast)
    pub is_recording: bool,
    pub file_ext: String,
}

/// A link ready for download
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub url: String,
    pub filename: String,
}

/// History entry for a past download
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub time: i64,
    pub links: Vec<String>,
    pub filenames: Vec<String>,
    pub video_dirname: String,
}

/// Saved credentials from config.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedConfig {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub remember_username: bool,
    #[serde(default)]
    pub remember_password: bool,
    #[serde(default)]
    pub course_id: Option<String>,
}

/// Exported course data (import/export)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseExport {
    pub courses: Vec<Course>,
}
