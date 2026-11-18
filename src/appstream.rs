//! Read the human-friendly description from AppStream metainfo files
//! (the text software centres show), and format Debian long descriptions.

fn decode(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&")
}

/// Extract the untranslated <description> and the homepage URL.
pub fn parse(xml: &str) -> (String, Option<String>) {
    let homepage = xml.find("<url type=\"homepage\">").and_then(|i| {
        let rest = &xml[i + "<url type=\"homepage\">".len()..];
        rest.find("</url>").map(|e| decode(rest[..e].trim()))
    });
    // First <description> without xml:lang (the original language).
    let mut desc = String::new();
    let mut search = 0;
    while let Some(i) = xml[search..].find("<description") {
        let start = search + i;
        let Some(tag_end) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + tag_end];
        let Some(close) = xml[start..].find("</description>") else { break };
        if !tag.contains("xml:lang") {
            desc = render(&xml[start + tag_end + 1..start + close]);
            break;
        }
        search = start + close;
    }
    (desc, homepage)
}

/// Turn <p>, <ul><li> markup into plain text, skipping translated elements.
fn render(inner: &str) -> String {
    let mut out = String::new();
    let mut rest = inner;
    let mut skip_until: Option<String> = None;
    while let Some(lt) = rest.find('<') {
        let text = &rest[..lt];
        if skip_until.is_none() {
            let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                if !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(&decode(&t));
            }
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.trim_start_matches('/').split_whitespace().next().unwrap_or("").to_string();
        if let Some(end) = &skip_until {
            if tag.starts_with('/') && &name == end {
                skip_until = None;
            }
        } else if !tag.starts_with('/') && tag.contains("xml:lang") {
            skip_until = Some(name);
        } else {
            match (tag.starts_with('/'), name.as_str()) {
                (false, "p") | (false, "ul") | (false, "ol") | (true, "ul") | (true, "ol") => {
                    // exactly one blank line between blocks
                    if !out.is_empty() {
                        let trimmed = out.trim_end_matches([' ', '\n']).len();
                        out.truncate(trimmed);
                        out.push_str("\n\n");
                    }
                }
                (false, "li") => {
                    if !out.ends_with('\n') && !out.is_empty() {
                        let trimmed = out.trim_end_matches(' ').len();
                        out.truncate(trimmed);
                        out.push('\n');
                    }
                    out.push_str("• ");
                }
                _ => {}
            }
        }
        rest = &rest[lt + gt + 1..];
    }
    let cleaned: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    cleaned.join("\n").replace("\n\n\n", "\n\n").trim().to_string()
}

/// Debian-style long description: lines after the first, " ." = blank line.
pub fn debian_long(desc: &str) -> String {
    let mut out = String::new();
    for l in desc.lines().skip(1) {
        let t = l.strip_prefix(' ').unwrap_or(l);
        if t.trim() == "." {
            out.push_str("\n\n");
        } else if t.starts_with(' ') || t.starts_with("- ") || t.starts_with("* ") {
            // pre-formatted / list line
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(t.trim_start());
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(t.trim());
        }
    }
    out.trim().replace("\n\n\n", "\n\n")
}

/// Find and parse a metainfo file among a package's files.
pub fn from_files(files: &[String]) -> Option<(String, Option<String>)> {
    files
        .iter()
        .filter(|f| (f.contains("/metainfo/") || f.contains("/appdata/")) && f.ends_with(".xml"))
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .map(|x| parse(&x))
        .find(|(d, _)| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metainfo() {
        let xml = r#"<component><url type="homepage">https://ex.org</url>
<description xml:lang="de"><p>Hallo</p></description>
<description><p>Edits &amp; views files.</p><p xml:lang="fr">Bonjour</p>
<ul><li>Fast</li><li>Small</li></ul><p>Free.</p></description></component>"#;
        let (d, h) = parse(xml);
        assert_eq!(h.as_deref(), Some("https://ex.org"));
        assert_eq!(d, "Edits & views files.\n\n• Fast\n• Small\n\nFree.");
    }

    #[test]
    fn debian() {
        let d = "GUI editor\n Line one\n continues.\n .\n Controls:\n  - Create\n  - Edit\n";
        assert_eq!(debian_long(d), "Line one continues.\n\nControls:\n- Create\n- Edit");
    }
}
