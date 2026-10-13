use std::ops::Range;

use gpui_kit::base::{Link, SelectableText, TestSupportExt as _, TextSelection};
use gpui_kit::component::{ActiveTheme as _, h_flex, tooltip::Tooltip, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::BeaconTheme as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebLink {
    pub range: Range<usize>,
    pub url: String,
}

/// Find explicit HTTP(S) URLs without changing the displayed or copied value.
pub(crate) fn detect(text: &str) -> Vec<WebLink> {
    let mut links = Vec::new();
    let mut offset = 0;
    let lowercase = text.to_ascii_lowercase();
    while offset < text.len() {
        let rest = &lowercase[offset..];
        let Some(start) = [rest.find("https://"), rest.find("http://")]
            .into_iter()
            .flatten()
            .min()
        else {
            break;
        };
        let start = offset + start;
        let end = text[start..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | ',' | '}')
            })
            .map_or(text.len(), |end| start + end);
        let mut candidate = text[start..end].trim_end_matches(['.', ';']);
        for (open, close) in [('(', ')'), ('[', ']')] {
            while candidate.ends_with(close)
                && candidate.matches(close).count() > candidate.matches(open).count()
            {
                candidate = &candidate[..candidate.len() - 1];
            }
        }
        if let Ok(url) = url::Url::parse(candidate)
            && url.host_str().is_some()
            && !url.host_str().is_some_and(|host| host.contains('*'))
        {
            links.push(WebLink {
                range: start..start + candidate.len(),
                url: url.into(),
            });
        }
        offset = end;
    }
    links
}

pub(crate) fn render(text: String, links: &[WebLink], cx: &App) -> AnyElement {
    render_value(text, links, true, cx)
}

/// Fixed-height table cells keep every destination on the same line. Each
/// link can shrink and ellipsize within its column, with its full URL on hover.
pub(crate) fn table(text: String, links: &[WebLink], cx: &App) -> AnyElement {
    render_value(text, links, false, cx)
}

fn render_value(text: String, links: &[WebLink], wrap: bool, cx: &App) -> AnyElement {
    if links.is_empty() {
        return div()
            .min_w_0()
            .w_full()
            .when(!wrap, |this| this.truncate())
            .child(SelectableText::new("value", text))
            .into_any_element();
    }
    let mut offset = 0;
    v_flex()
        .w_full()
        .min_w_0()
        .children(text.split('\n').enumerate().map(|(line_index, line)| {
            let line_start = offset;
            offset += line.len() + 1;
            let mut parts = Vec::new();
            let mut cursor = 0;
            for link in links.iter().filter(|link| {
                link.range.start >= line_start && link.range.end <= line_start + line.len()
            }) {
                let start = link.range.start - line_start;
                let end = link.range.end - line_start;
                if start > cursor {
                    parts.push(
                        div()
                            .flex_shrink_0()
                            .child(SelectableText::new(
                                ("text", cursor),
                                line[cursor..start].to_string(),
                            ))
                            .into_any_element(),
                    );
                }
                let label = line[start..end].to_string();
                let url = link.url.clone();
                parts.push(
                    div()
                        .id(("web-link", start))
                        .test_support()
                        .min_w_0()
                        .when(!wrap, |this| this.flex_1().flex_basis(px(0.)).truncate())
                        .child(
                            Link::new("open-url")
                                .href(url.clone())
                                .accessibility_label(format!("Open {url}"))
                                .min_w_0()
                                .when(!wrap, |this| this.w_full().truncate())
                                .cursor_pointer()
                                .text_color(cx.theme().external_link())
                                .hover(|this| this.underline())
                                .tooltip(move |window, cx| {
                                    Tooltip::new(url.clone()).build(window, cx)
                                })
                                .child(SelectableText::new("label", label))
                                .open_with(|url, event, window, cx| {
                                    // Consuming the click also consumes MouseUp; finish the
                                    // selection gesture before blocking the table row's click.
                                    let selected = TextSelection::selected_text(window, cx);
                                    TextSelection::end(window, cx);
                                    cx.stop_propagation();
                                    if matches!(event, ClickEvent::Mouse(_)) && !selected.is_empty()
                                    {
                                        return;
                                    }
                                    cx.open_url(url);
                                }),
                        )
                        .into_any_element(),
                );
                cursor = end;
            }
            if cursor < line.len() {
                parts.push(
                    div()
                        .flex_shrink_0()
                        .child(SelectableText::new(
                            ("text", cursor),
                            line[cursor..].to_string(),
                        ))
                        .into_any_element(),
                );
            }
            h_flex()
                .id(("line", line_index))
                .min_w_0()
                .w_full()
                .when(wrap, |this| this.flex_wrap())
                .when(!wrap, |this| {
                    this.flex_nowrap().overflow_hidden().whitespace_nowrap()
                })
                .items_baseline()
                .children(parts)
        }))
        .into_any_element()
}

/// Rich text keeps an entire key=value entry in one selection participant,
/// so copying across inline links does not insert extra line breaks.
pub(crate) fn inline(text: String, links: &[WebLink], cx: &App) -> AnyElement {
    if links.is_empty() {
        return SelectableText::new("value", text).into_any_element();
    }
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let mut html = String::new();
    let mut cursor = 0;
    for link in links {
        html.push_str(&escape(&text[cursor..link.range.start]));
        html.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            escape(&link.url),
            escape(&text[link.range.clone()])
        ));
        cursor = link.range.end;
    }
    html.push_str(&escape(&text[cursor..]));
    gpui_kit::base::TextView::html("value", html.replace('\n', "<br>"))
        .style(
            gpui_kit::base::TextViewStyle::default()
                .with_foreground(cx.theme().foreground)
                .with_link(cx.theme().external_link()),
        )
        .on_link_click(|url, _, _, cx| cx.open_url(url))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn annotation_urls_keep_byte_ranges_and_individual_destinations() {
        let text = "文档=https://example.com/docs, http://other.example/api?x=1&y=2\n说明";
        let links = detect(text);
        assert_eq!(links.len(), 2);
        assert_eq!(&text[links[0].range.clone()], "https://example.com/docs");
        assert_eq!(
            &text[links[1].range.clone()],
            "http://other.example/api?x=1&y=2"
        );
        assert_eq!(links[1].url, "http://other.example/api?x=1&y=2");
        assert!(detect("javascript:alert(1) https://*.example.com/").is_empty());
        let text = "文档 (HTTPS://example.com/docs)";
        assert_eq!(
            &text[detect(text)[0].range.clone()],
            "HTTPS://example.com/docs"
        );
    }
}
