use regex::Regex;
use std::sync::LazyLock;

static RAR_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.r(\d+)$").expect("rar ext regex"));

static MULTIPART_MKV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.\d{3}$").expect("multipart mkv regex"));

pub fn is_rar_file(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".rar") || RAR_EXT_RE.is_match(filename)
}

pub fn is_7z_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".7z") || lower.contains(".7z.")
}

pub fn is_multipart_mkv(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    (lower.contains(".mkv.") || lower.contains(".avi.") || lower.contains(".mp4."))
        && MULTIPART_MKV_RE.is_match(&lower)
}

pub fn is_video_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    matches!(
        std::path::Path::new(&lower)
            .extension()
            .and_then(|e| e.to_str()),
        Some("mkv" | "avi" | "mp4" | "wmv" | "ts" | "m4v" | "mov" | "webm")
    )
}

pub fn is_par2_file(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".par2")
}

pub fn is_important_file_type(filename: &str) -> bool {
    is_video_file(filename) || is_rar_file(filename) || is_7z_file(filename)
}

pub fn is_probably_obfuscated(filename: &str) -> bool {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    // Pure hex hash (32+ chars)
    if stem.len() >= 32 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // UUID pattern
    if uuid::Uuid::try_parse(stem).is_ok() {
        return true;
    }
    // Pure numeric
    if stem.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    false
}

/// Extract password from NZB filename, e.g. "release {{password}}.nzb"
pub fn get_nzb_password(filename: &str) -> Option<String> {
    static PW_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\{\{(.+?)\}\}").expect("password regex"));
    PW_RE.captures(filename).map(|c| c[1].to_string())
}

/// Extract job name from NZB filename (strip extension and password).
pub fn get_job_name(filename: &str) -> String {
    let name = filename
        .trim_end_matches(".nzb")
        .trim_end_matches(".gz")
        .trim_end_matches(".nzb");
    // Strip password marker
    static PW_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s*\{\{.+?\}\}\s*").expect("password strip regex"));
    PW_RE.replace_all(name, "").trim().to_string()
}

pub fn matches_blocklist(filename: &str, patterns: &[String]) -> bool {
    let lower = filename.to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        glob::Pattern::new(&pattern.to_ascii_lowercase())
            .map(|p| p.matches(&lower))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rar_file() {
        assert!(is_rar_file("archive.rar"));
        assert!(is_rar_file("archive.RAR"));
        assert!(is_rar_file("archive.r00"));
        assert!(is_rar_file("archive.r01"));
        assert!(is_rar_file("movie.part01.rar"));
        assert!(!is_rar_file("readme.txt"));
        assert!(!is_rar_file("video.mkv"));
    }

    #[test]
    fn test_is_par2_file() {
        assert!(is_par2_file("file.par2"));
        assert!(is_par2_file("file.vol01+02.par2"));
        assert!(is_par2_file("FILE.PAR2"));
        assert!(!is_par2_file("file.txt"));
        assert!(!is_par2_file("file.par"));
    }

    #[test]
    fn test_is_video_file() {
        assert!(is_video_file("movie.mkv"));
        assert!(is_video_file("movie.mp4"));
        assert!(is_video_file("movie.avi"));
        assert!(is_video_file("movie.MKV"));
        assert!(is_video_file("movie.wmv"));
        assert!(is_video_file("movie.ts"));
        assert!(!is_video_file("info.nfo"));
        assert!(!is_video_file("readme.txt"));
        assert!(!is_video_file("archive.rar"));
    }

    #[test]
    fn test_is_probably_obfuscated() {
        // 32-char hex hash
        assert!(is_probably_obfuscated(
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4.mkv"
        ));
        // UUID
        assert!(is_probably_obfuscated(
            "550e8400-e29b-41d4-a716-446655440000.mkv"
        ));
        // Pure numeric
        assert!(is_probably_obfuscated("1234567890.mkv"));
        // Normal name should not be obfuscated
        assert!(!is_probably_obfuscated("movie.name.mkv"));
        assert!(!is_probably_obfuscated("Some.Movie.2024.1080p.mkv"));
    }

    #[test]
    fn test_get_job_name() {
        assert_eq!(get_job_name("release.nzb"), "release");
        assert_eq!(get_job_name("release {{password}}.nzb"), "release");
        assert_eq!(get_job_name("file.nzb.gz"), "file");
        assert_eq!(get_job_name("noext"), "noext");
    }

    #[test]
    fn test_get_nzb_password_double_braces() {
        assert_eq!(
            get_nzb_password("release {{mypass}}.nzb"),
            Some("mypass".to_string())
        );
    }

    #[test]
    fn test_get_nzb_password_no_password() {
        assert_eq!(get_nzb_password("release.nzb"), None);
    }

    #[test]
    fn test_get_nzb_password_empty_braces() {
        // The regex `.+?` requires at least one character, so empty braces
        // should return None.
        assert_eq!(get_nzb_password("release {{}}.nzb"), None);
    }

    #[test]
    fn test_matches_blocklist() {
        let patterns = vec!["*.nfo".to_string(), "*.txt".to_string()];
        assert!(matches_blocklist("info.nfo", &patterns));
        assert!(matches_blocklist("readme.txt", &patterns));
        assert!(matches_blocklist("INFO.NFO", &patterns));
        assert!(!matches_blocklist("movie.mkv", &patterns));
        assert!(!matches_blocklist("archive.rar", &patterns));
    }
}
