//! Minimal XML helpers shared by the SOAP and topology layers.
//!
//! Two needs are covered: escaping dynamic values before they are embedded in a
//! SOAP envelope or DIDL-Lite document, and extracting the (unescaped) text of a
//! named element from a response — matching on the *local* name so namespace
//! prefixes such as `s:` / `u:` do not matter.

use quick_xml::events::Event;
use quick_xml::Reader;

/// Escape the five XML metacharacters so `input` is safe as element text or an
/// attribute value.
pub(crate) fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Return the unescaped text content of the first element whose local name
/// equals `local_name`, or `None` if no such element exists.
///
/// Only character data is collected; this is intended for leaf elements (SOAP
/// fault fields, the `ZoneGroupState` string out-argument, etc.).
pub(crate) fn first_element_text(xml: &str, local_name: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let target = local_name.as_bytes();

    let mut inside = false;
    let mut depth = 0usize;
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if inside {
                    depth += 1;
                } else if e.local_name().as_ref() == target {
                    inside = true;
                    depth = 0;
                }
            }
            Ok(Event::End(_)) => {
                if inside {
                    if depth == 0 {
                        return Some(text);
                    }
                    depth -= 1;
                }
            }
            // In quick-xml 0.40 entity references (`&lt;`, `&#60;`, …) arrive as
            // their own `GeneralRef` events, so plain `Text` is already literal.
            Ok(Event::Text(e)) if inside => {
                if let Ok(decoded) = e.decode() {
                    text.push_str(&decoded);
                }
            }
            Ok(Event::GeneralRef(e)) if inside => {
                if let Ok(name) = e.decode() {
                    let entity = format!("&{name};");
                    if let Ok(resolved) = quick_xml::escape::unescape(&entity) {
                        text.push_str(&resolved);
                    }
                }
            }
            Ok(Event::CData(e)) if inside => {
                text.push_str(&String::from_utf8_lossy(&e.into_inner()));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_all_metacharacters() {
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn extracts_text_by_local_name_ignoring_prefix() {
        let xml = r#"<s:Body><u:Resp xmlns:u="x"><errorCode>701</errorCode></u:Resp></s:Body>"#;
        assert_eq!(first_element_text(xml, "errorCode").as_deref(), Some("701"));
    }

    #[test]
    fn unescapes_nested_xml_string_argument() {
        let xml = "<ZoneGroupState>&lt;ZoneGroups&gt;&lt;/ZoneGroups&gt;</ZoneGroupState>";
        assert_eq!(
            first_element_text(xml, "ZoneGroupState").as_deref(),
            Some("<ZoneGroups></ZoneGroups>")
        );
    }

    #[test]
    fn returns_none_for_missing_element() {
        assert!(first_element_text("<a><b/></a>", "missing").is_none());
    }
}
