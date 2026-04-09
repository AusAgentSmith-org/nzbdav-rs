//! PROPFIND XML response generation (DAV:multistatus).

use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};

use crate::store::DavNode;

/// Generate a `DAV:multistatus` XML response for a PROPFIND request.
///
/// `nodes` should contain the target resource and (for depth-1) its children.
/// `base_href` is the request path used to build the `<D:href>` elements.
pub fn multistatus_xml(nodes: &[DavNode], base_href: &str) -> String {
    let mut buf = Vec::with_capacity(4096);
    let mut writer = Writer::new_with_indent(&mut buf, b' ', 2);

    // XML declaration
    writer
        .write_event(Event::Decl(quick_xml::events::BytesDecl::new(
            "1.0",
            Some("utf-8"),
            None,
        )))
        .expect("xml decl");

    // <D:multistatus xmlns:D="DAV:">
    let mut ms = BytesStart::new("D:multistatus");
    ms.push_attribute(("xmlns:D", "DAV:"));
    writer
        .write_event(Event::Start(ms))
        .expect("multistatus start");

    for (i, node) in nodes.iter().enumerate() {
        let href = if i == 0 {
            base_href.to_string()
        } else {
            node.item.path.clone()
        };
        write_response_element(&mut writer, node, &href);
    }

    // </D:multistatus>
    writer
        .write_event(Event::End(BytesEnd::new("D:multistatus")))
        .expect("multistatus end");

    String::from_utf8(buf).expect("valid utf-8")
}

fn write_response_element<W: std::io::Write>(writer: &mut Writer<W>, node: &DavNode, href: &str) {
    // <D:response>
    writer
        .write_event(Event::Start(BytesStart::new("D:response")))
        .expect("response start");

    // <D:href>
    write_text_element(writer, "D:href", href);

    // <D:propstat>
    writer
        .write_event(Event::Start(BytesStart::new("D:propstat")))
        .expect("propstat start");

    // <D:prop>
    writer
        .write_event(Event::Start(BytesStart::new("D:prop")))
        .expect("prop start");

    // <D:displayname>
    write_text_element(writer, "D:displayname", &node.item.name);

    // <D:resourcetype>
    if node.is_collection {
        writer
            .write_event(Event::Start(BytesStart::new("D:resourcetype")))
            .expect("resourcetype start");
        writer
            .write_event(Event::Empty(BytesStart::new("D:collection")))
            .expect("collection");
        writer
            .write_event(Event::End(BytesEnd::new("D:resourcetype")))
            .expect("resourcetype end");
    } else {
        writer
            .write_event(Event::Empty(BytesStart::new("D:resourcetype")))
            .expect("resourcetype empty");
    }

    // File-specific properties
    if !node.is_collection {
        if let Some(size) = node.item.file_size {
            write_text_element(writer, "D:getcontentlength", &size.to_string());
        }
        write_text_element(writer, "D:getcontenttype", &node.content_type);
    }

    // <D:getetag>
    write_text_element(writer, "D:getetag", &node.etag);

    // <D:creationdate> (ISO 8601)
    let created = node
        .item
        .created_at
        .and_utc()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    write_text_element(writer, "D:creationdate", &created);

    // <D:getlastmodified> (RFC 2822)
    let modified = node
        .item
        .created_at
        .and_utc()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    write_text_element(writer, "D:getlastmodified", &modified);

    // </D:prop>
    writer
        .write_event(Event::End(BytesEnd::new("D:prop")))
        .expect("prop end");

    // <D:status>HTTP/1.1 200 OK</D:status>
    write_text_element(writer, "D:status", "HTTP/1.1 200 OK");

    // </D:propstat>
    writer
        .write_event(Event::End(BytesEnd::new("D:propstat")))
        .expect("propstat end");

    // </D:response>
    writer
        .write_event(Event::End(BytesEnd::new("D:response")))
        .expect("response end");
}

fn write_text_element<W: std::io::Write>(writer: &mut Writer<W>, tag: &str, text: &str) {
    writer
        .write_event(Event::Start(BytesStart::new(tag)))
        .expect("start");
    writer
        .write_event(Event::Text(BytesText::new(text)))
        .expect("text");
    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .expect("end");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nzbdav_core::models::{DavItem, ItemSubType, ItemType};
    use uuid::Uuid;

    fn make_dir_node(name: &str, path: &str) -> DavNode {
        DavNode::from_item(DavItem {
            id: Uuid::nil(),
            id_prefix: "t".into(),
            created_at: Utc::now().naive_utc(),
            parent_id: None,
            name: name.into(),
            file_size: None,
            item_type: ItemType::Directory,
            sub_type: ItemSubType::Directory,
            path: path.into(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        })
    }

    fn make_file_node(name: &str, path: &str, size: i64) -> DavNode {
        DavNode::from_item(DavItem {
            id: Uuid::nil(),
            id_prefix: "t".into(),
            created_at: Utc::now().naive_utc(),
            parent_id: None,
            name: name.into(),
            file_size: Some(size),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::NzbFile,
            path: path.into(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        })
    }

    #[test]
    fn test_multistatus_contains_response() {
        let node = make_dir_node("root", "/");
        let xml = multistatus_xml(&[node], "/");
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("<D:response>"));
        assert!(xml.contains("<D:collection/>"));
        assert!(xml.contains("HTTP/1.1 200 OK"));
    }

    #[test]
    fn test_multistatus_file_node() {
        let node = make_file_node("movie.mkv", "/content/movie.mkv", 1048576);
        let xml = multistatus_xml(&[node], "/content/movie.mkv");
        assert!(
            xml.contains("<D:getcontentlength>1048576</D:getcontentlength>"),
            "should contain content length"
        );
        assert!(
            xml.contains("<D:getcontenttype>"),
            "should contain content type"
        );
        // File nodes should NOT have <D:collection/>
        assert!(
            !xml.contains("<D:collection/>"),
            "file node should not be a collection"
        );
        // Should have an empty resourcetype
        assert!(
            xml.contains("<D:resourcetype/>"),
            "file should have empty resourcetype"
        );
    }

    #[test]
    fn test_multistatus_depth1() {
        let parent = make_dir_node("content", "/content/");
        let child1 = make_dir_node("movies", "/content/movies/");
        let child2 = make_file_node("file.mkv", "/content/file.mkv", 500);
        let xml = multistatus_xml(&[parent, child1, child2], "/content/");

        // Count response elements
        let response_count = xml.matches("<D:response>").count();
        assert_eq!(response_count, 3, "should have 3 response elements");

        // First href should be the request path
        assert!(xml.contains("<D:href>/content/</D:href>"));
        // Children use their own path
        assert!(xml.contains("<D:href>/content/movies/</D:href>"));
        assert!(xml.contains("<D:href>/content/file.mkv</D:href>"));
    }

    #[test]
    fn test_multistatus_xml_escaping() {
        let node = make_dir_node("Tom & Jerry <2024>", "/content/Tom & Jerry <2024>/");
        let xml = multistatus_xml(&[node], "/content/Tom & Jerry <2024>/");
        // quick_xml should escape & and < in text content
        assert!(
            xml.contains("&amp;") || xml.contains("Tom &amp; Jerry"),
            "ampersand should be escaped in XML"
        );
        assert!(
            xml.contains("&lt;") || xml.contains("&lt;2024&gt;"),
            "angle brackets should be escaped in XML"
        );
        // Verify XML is well-formed by checking it doesn't contain unescaped problematic chars in text
        assert!(
            !xml.contains("<D:displayname>Tom & Jerry <2024></D:displayname>"),
            "special chars must be escaped"
        );
    }
}
