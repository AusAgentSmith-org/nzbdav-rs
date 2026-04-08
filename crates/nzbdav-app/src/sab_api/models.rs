use serde::Serialize;

#[derive(Serialize)]
pub struct QueueResponse {
    pub queue: QueueData,
}

#[derive(Serialize)]
pub struct QueueData {
    /// "Downloading" / "Idle" / "Paused"
    pub status: String,
    pub noofslots: usize,
    pub noofslots_total: usize,
    pub slots: Vec<QueueSlot>,
}

#[derive(Serialize)]
pub struct QueueSlot {
    pub nzo_id: String,
    pub filename: String,
    pub cat: String,
    /// "Normal", "High", "Low", "Force"
    pub priority: String,
    /// "Queued", "Downloading", "Paused"
    pub status: String,
    /// Total size in MB.
    pub mb: String,
    /// Remaining in MB (always "0" for nzbdav -- no real download).
    pub mbleft: String,
    /// Always "100" (nzbdav doesn't download).
    pub percentage: String,
    /// Always "0:00:00".
    pub timeleft: String,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub history: HistoryData,
}

#[derive(Serialize)]
pub struct HistoryData {
    pub noofslots: usize,
    pub slots: Vec<HistorySlot>,
}

#[derive(Serialize)]
pub struct HistorySlot {
    pub nzo_id: String,
    pub name: String,
    pub category: String,
    /// "Completed" or "Failed"
    pub status: String,
    pub fail_message: String,
    pub bytes: i64,
    pub download_time: i32,
    /// Unix timestamp.
    pub completed: i64,
    /// Path where files are (WebDAV path).
    pub storage: String,
}

#[derive(Serialize)]
pub struct AddFileResponse {
    pub status: bool,
    pub nzo_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub status: String,
    pub version: String,
    pub paused: bool,
}

#[derive(Serialize)]
pub struct CategoriesResponse {
    pub categories: Vec<String>,
}

#[derive(Serialize)]
pub struct SimpleResponse {
    pub status: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
