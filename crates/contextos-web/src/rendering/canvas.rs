//! `.canvas` SVG rendering (`web-rendering.md` §3, FR-223, FR-243): a JSON
//! Canvas 1.0 document (`obsidian:json-canvas` skill), laid out to SVG
//! using each node's own `x`/`y`/`width`/`height`. No auto-layout runs at
//! any point: every coordinate used is the one the file itself already
//! carries.
//!
//! A `canvas_read` diagnostic (dangling edge, duplicate id, unparseable
//! file, `D-31`) is the caller's signal to render
//! [`diagnostics::render_diagnostic_panel`](super::diagnostics::render_diagnostic_panel)
//! instead of calling [`render_svg`] at all; this module assumes it is
//! only ever asked to lay out an already-diagnostic-free node/edge set.

use serde::Deserialize;

use crate::rendering::escape_html;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CanvasNode {
    Text {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text: String,
        #[serde(default)]
        color: Option<String>,
    },
    File {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        file: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    Link {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        url: String,
        #[serde(default)]
        color: Option<String>,
    },
    Group {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        background: Option<String>,
        #[serde(default, rename = "backgroundStyle")]
        background_style: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
}

impl CanvasNode {
    #[must_use]
    fn geometry(&self) -> (f64, f64, f64, f64) {
        match self {
            Self::Text {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::File {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::Link {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::Group {
                x,
                y,
                width,
                height,
                ..
            } => (*x, *y, *width, *height),
        }
    }

    #[must_use]
    fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::File { id, .. }
            | Self::Link { id, .. }
            | Self::Group { id, .. } => id,
        }
    }

    #[must_use]
    fn color(&self) -> Option<&str> {
        match self {
            Self::Text { color, .. }
            | Self::File { color, .. }
            | Self::Link { color, .. }
            | Self::Group { color, .. } => color.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CanvasEdge {
    pub id: String,
    #[serde(rename = "fromNode")]
    pub from_node: String,
    #[serde(default, rename = "fromSide")]
    pub from_side: Option<String>,
    #[serde(default, rename = "fromEnd")]
    pub from_end: Option<String>,
    #[serde(rename = "toNode")]
    pub to_node: String,
    #[serde(default, rename = "toSide")]
    pub to_side: Option<String>,
    #[serde(default, rename = "toEnd")]
    pub to_end: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Maps a JSON Canvas preset colour (`"1"` to `"6"`) or an already-literal
/// hex value to a CSS colour, per the `obsidian:json-canvas` skill's
/// documented preset palette. Preset-to-actual-colour mapping is a
/// `contextos-web` UI theme decision (`web-rendering.md` §3), not part of
/// the format itself.
fn resolve_color(color: Option<&str>, fallback: &str) -> String {
    match color {
        None => fallback.to_owned(),
        Some("1") => "#a13a3a".to_owned(),
        Some("2") => "#a5651f".to_owned(),
        Some("3") => "#a68a1f".to_owned(),
        Some("4") => "#5b6b4f".to_owned(),
        Some("5") => "#3f7a8a".to_owned(),
        Some("6") => "#5a4f8a".to_owned(),
        Some(hex) => hex.to_owned(),
    }
}

/// Renders `text` (a canvas text node's own Markdown content,
/// `web-rendering.md` §3) to HTML. Reuses only the synchronous subset of
/// the stage-1 Markdown pipeline (general Markdown, triple-colon fences,
/// callouts): a canvas text node's own wikilinks are not resolved in v1,
/// rendering instead as their literal bracketed text, a deliberate,
/// disclosed scope limit rather than threading an MCP round trip through
/// every node of every rendered canvas.
fn render_node_text(text: &str) -> String {
    let fences_extracted = super::fences::extract(text);
    let callouts_extracted = super::callouts::extract(&fences_extracted.text);
    let mut html = super::markdown::html_from_commonmark(&callouts_extracted.text);
    for (index, block) in callouts_extracted.blocks.iter().enumerate() {
        let body_html = super::markdown::html_from_commonmark(&block.body);
        let rendered = super::callouts::render(&block.open, &body_html);
        html = html.replace(
            &format!("<p>{}</p>", super::callouts::placeholder(index)),
            &rendered,
        );
    }
    for (index, block) in fences_extracted.blocks.iter().enumerate() {
        let inner_html = super::markdown::html_from_commonmark(&block.inner);
        let rendered = super::fences::render(&block.open, &inner_html);
        html = html.replace(
            &format!("<p>{}</p>", super::fences::placeholder(index)),
            &rendered,
        );
    }
    html
}

/// Renders a full `.canvas` document to SVG. `viewport` is the SVG's own
/// `width`/`height` attributes (the rendered frame's CSS box, not the
/// canvas's content bounds, which the returned `viewBox` covers instead).
#[must_use]
pub fn render_svg(nodes: &[CanvasNode], edges: &[CanvasEdge], vault_name: &str) -> String {
    let (min_x, min_y, max_x, max_y) = bounds(nodes);
    let padding = 40.0;
    let view_box = format!(
        "{} {} {} {}",
        min_x - padding,
        min_y - padding,
        (max_x - min_x) + 2.0 * padding,
        (max_y - min_y) + 2.0 * padding
    );

    let mut body = String::new();
    for node in nodes {
        body.push_str(&render_node(node, vault_name));
    }
    for edge in edges {
        if let Some(svg) = render_edge(edge, nodes) {
            body.push_str(&svg);
        }
    }

    format!(
        "<svg class=\"canvas-svg\" viewBox=\"{view_box}\" xmlns=\"http://www.w3.org/2000/svg\">\
<defs><marker id=\"canvas-arrow\" markerWidth=\"8\" markerHeight=\"8\" refX=\"6\" refY=\"3\" orient=\"auto\">\
<path d=\"M0,0 L6,3 L0,6 Z\" fill=\"currentColor\"/></marker></defs>\
{body}</svg>"
    )
}

fn bounds(nodes: &[CanvasNode]) -> (f64, f64, f64, f64) {
    if nodes.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for node in nodes {
        let (x, y, w, h) = node.geometry();
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    (min_x, min_y, max_x, max_y)
}

fn render_node(node: &CanvasNode, vault_name: &str) -> String {
    let (x, y, w, h) = node.geometry();
    let id = escape_html(node.id());
    match node {
        CanvasNode::Group {
            label, background, ..
        } => {
            let stroke = resolve_color(node.color(), "#b8b2a5");
            let fill = background.clone().unwrap_or_else(|| "none".to_owned());
            let label_html = label
                .as_ref()
                .map(|l| {
                    format!(
                        "<text x=\"{}\" y=\"{}\" class=\"canvas-group-label\">{}</text>",
                        x + 6.0,
                        y - 6.0,
                        escape_html(l)
                    )
                })
                .unwrap_or_default();
            format!(
                "<g class=\"canvas-node canvas-node-group\" data-id=\"{id}\">\
<rect class=\"canvas-group-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"{fill}\" stroke=\"{stroke}\" stroke-dasharray=\"4 3\"/>{label_html}</g>"
            )
        }
        CanvasNode::Text { text, .. } => {
            let stroke = resolve_color(node.color(), "#b8b2a5");
            let content = render_node_text(text);
            format!(
                "<g class=\"canvas-node canvas-node-text\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>\
<foreignObject x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\">\
<div xmlns=\"http://www.w3.org/1999/xhtml\" class=\"canvas-node-text\">{content}</div>\
</foreignObject></g>"
            )
        }
        CanvasNode::File { file, subpath, .. } => {
            let stroke = resolve_color(node.color(), "#b8b2a5");
            let href = match subpath {
                Some(sub) => format!("/{vault_name}/{file}#{sub}"),
                None => format!("/{vault_name}/{file}"),
            };
            format!(
                "<g class=\"canvas-node canvas-node-file\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>\
<a href=\"{href}\"><text x=\"{tx}\" y=\"{ty}\" class=\"canvas-node-text\">{label}</text></a></g>",
                href = escape_html(&href),
                tx = x + w / 2.0,
                ty = y + h / 2.0,
                label = escape_html(file),
            )
        }
        CanvasNode::Link { url, .. } => {
            let stroke = resolve_color(node.color(), "#b8b2a5");
            format!(
                "<g class=\"canvas-node canvas-node-link\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>\
<a href=\"{href}\" target=\"_blank\" rel=\"noopener noreferrer\">\
<text x=\"{tx}\" y=\"{ty}\" class=\"canvas-node-text\">{label}</text></a></g>",
                href = escape_html(url),
                tx = x + w / 2.0,
                ty = y + h / 2.0,
                label = escape_html(url),
            )
        }
    }
}

fn render_edge(edge: &CanvasEdge, nodes: &[CanvasNode]) -> Option<String> {
    let from = nodes.iter().find(|n| n.id() == edge.from_node)?;
    let to = nodes.iter().find(|n| n.id() == edge.to_node)?;
    let (fx, fy, fw, fh) = from.geometry();
    let (tx, ty, tw, th) = to.geometry();
    let start = anchor_point(fx, fy, fw, fh, edge.from_side.as_deref());
    let end = anchor_point(tx, ty, tw, th, edge.to_side.as_deref());
    let arrow_end = edge.to_end.as_deref().unwrap_or("arrow");
    let marker = if arrow_end == "none" {
        String::new()
    } else {
        " marker-end=\"url(#canvas-arrow)\"".to_owned()
    };
    let color = resolve_color(edge.color.as_deref(), "#9a9488");
    let label_html = edge
        .label
        .as_ref()
        .map(|label| {
            format!(
                "<text x=\"{}\" y=\"{}\" class=\"canvas-edge-label\">{}</text>",
                f64::midpoint(start.0, end.0),
                f64::midpoint(start.1, end.1) - 4.0,
                escape_html(label)
            )
        })
        .unwrap_or_default();
    Some(format!(
        "<path class=\"canvas-edge\" d=\"M{},{} L{},{}\" stroke=\"{color}\" fill=\"none\"{marker}/>{label_html}",
        start.0, start.1, end.0, end.1
    ))
}

fn anchor_point(x: f64, y: f64, w: f64, h: f64, side: Option<&str>) -> (f64, f64) {
    match side {
        Some("top") => (x + w / 2.0, y),
        Some("bottom") => (x + w / 2.0, y + h),
        Some("left") => (x, y + h / 2.0),
        Some("right") | None => (x + w, y + h / 2.0),
        Some(_) => (x + w / 2.0, y + h / 2.0),
    }
}

#[cfg(test)]
#[path = "canvas_test.rs"]
mod tests;
