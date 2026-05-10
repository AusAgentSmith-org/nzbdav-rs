/// Convert an archive/NZB filename into a single DAV filename component.
///
/// The virtual tree stores files as direct children of the job directory. RAR
/// members can contain Windows or Unix path separators, so keep only the leaf
/// filename and drop unsafe path components before building `DavItem.path`.
pub(super) fn dav_leaf_name(raw_name: &str) -> String {
    let leaf = raw_name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty() && *part != "." && *part != "..")
        .unwrap_or("download.bin");

    let name: String = leaf
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();

    if name.is_empty() || name == "." || name == ".." {
        "download.bin".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_leaf_from_windows_archive_path() {
        let name = dav_leaf_name(
            r"1sAmXP9m2rQ8eEDY2Ko4m4g\ELiFENiC-462x.yaRulB.p0801.4991.noitpmedeR.knahswahS.ehT\The.Shawshank.Redemption.1994.1080p.BluRay.x264-CiNEFiLE .mkv",
        );

        assert_eq!(
            name,
            "The.Shawshank.Redemption.1994.1080p.BluRay.x264-CiNEFiLE .mkv"
        );
    }

    #[test]
    fn rejects_empty_or_parent_components() {
        assert_eq!(dav_leaf_name("../"), "download.bin");
        assert_eq!(dav_leaf_name("folder/../"), "folder");
    }

    #[test]
    fn removes_control_characters() {
        assert_eq!(dav_leaf_name("folder/movie\u{0}.mkv"), "movie.mkv");
    }
}
