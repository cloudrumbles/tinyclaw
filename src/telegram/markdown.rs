use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, CodeBlockKind};

/// Convert Markdown text to Telegram-compatible HTML.
///
/// Telegram supports a limited subset of HTML:
/// `<b>`, `<i>`, `<u>`, `<s>`, `<code>`, `<pre>`, `<a href="">`,
/// `<blockquote>`, `<tg-spoiler>`.
///
/// Unknown tags are stripped by Telegram, so this is forgiving.
pub fn markdown_to_telegram_html(markdown: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(markdown, options);

    let mut output = String::with_capacity(markdown.len());
    let mut in_code_block = false;
    let mut list_depth: u32 = 0;
    let mut ordered_indices: Vec<u64> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } => output.push_str("<b>"),
                Tag::Strong => output.push_str("<b>"),
                Tag::Emphasis => output.push_str("<i>"),
                Tag::Strikethrough => output.push_str("<s>"),
                Tag::BlockQuote(_) => output.push_str("<blockquote>"),
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    match kind {
                        CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                            output.push_str(&format!(
                                "<pre><code class=\"language-{}\">",
                                escape_html(&lang)
                            ));
                        }
                        _ => output.push_str("<pre><code>"),
                    }
                }
                Tag::Link { dest_url, .. } => {
                    output.push_str(&format!("<a href=\"{}\">", escape_html(&dest_url)));
                }
                Tag::List(start) => {
                    list_depth += 1;
                    if let Some(n) = start {
                        ordered_indices.push(n);
                    } else {
                        ordered_indices.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    if list_depth > 1 {
                        output.push_str("  ");
                    }
                    if let Some(idx) = ordered_indices.last_mut() {
                        if *idx > 0 {
                            output.push_str(&format!("{}. ", idx));
                            *idx += 1;
                        } else {
                            output.push_str("• ");
                        }
                    }
                }
                Tag::Paragraph => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    output.push_str("</b>\n");
                }
                TagEnd::Strong => output.push_str("</b>"),
                TagEnd::Emphasis => output.push_str("</i>"),
                TagEnd::Strikethrough => output.push_str("</s>"),
                TagEnd::BlockQuote(_) => output.push_str("</blockquote>"),
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    output.push_str("</code></pre>");
                }
                TagEnd::Link => output.push_str("</a>"),
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    ordered_indices.pop();
                }
                TagEnd::Item => {
                    output.push('\n');
                }
                TagEnd::Paragraph => {
                    output.push_str("\n\n");
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    output.push_str(&escape_html(&text));
                } else {
                    output.push_str(&escape_html(&text));
                }
            }
            Event::Code(code) => {
                output.push_str("<code>");
                output.push_str(&escape_html(&code));
                output.push_str("</code>");
            }
            Event::SoftBreak => output.push('\n'),
            Event::HardBreak => output.push('\n'),
            Event::Rule => output.push_str("\n---\n"),
            _ => {}
        }
    }

    // Trim trailing whitespace
    let trimmed = output.trim_end();
    trimmed.to_string()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_and_inline_code() {
        let md = "**Home** and `code`";
        let html = markdown_to_telegram_html(md);
        assert_eq!(html, "<b>Home</b> and <code>code</code>");
    }

    #[test]
    fn code_block_with_lang() {
        let md = "```rust\nfn main() {}\n```";
        let html = markdown_to_telegram_html(md);
        assert!(html.contains("<pre><code class=\"language-rust\">"));
        assert!(html.contains("fn main() {}"));
        assert!(html.contains("</code></pre>"));
    }

    #[test]
    fn unordered_list() {
        let md = "- one\n- two\n- three";
        let html = markdown_to_telegram_html(md);
        assert!(html.contains("• one"));
        assert!(html.contains("• two"));
        assert!(html.contains("• three"));
    }

    #[test]
    fn link() {
        let md = "[click here](https://example.com)";
        let html = markdown_to_telegram_html(md);
        assert_eq!(
            html,
            "<a href=\"https://example.com\">click here</a>"
        );
    }

    #[test]
    fn heading_becomes_bold() {
        let md = "# Title\nSome text";
        let html = markdown_to_telegram_html(md);
        assert!(html.contains("<b>Title</b>"));
        assert!(html.contains("Some text"));
    }

    #[test]
    fn html_entities_escaped() {
        let md = "x < y && z > w";
        let html = markdown_to_telegram_html(md);
        assert_eq!(html, "x &lt; y &amp;&amp; z &gt; w");
    }

    #[test]
    fn mixed_formatting() {
        let md = "**Home (`/home/tinyclaw/`)**\n- `sultana-workspace/` — my main workspace";
        let html = markdown_to_telegram_html(md);
        assert!(html.contains("<b>Home (<code>/home/tinyclaw/</code>)</b>"));
        assert!(html.contains("• <code>sultana-workspace/</code> — my main workspace"));
    }
}
