//! Markdown/OFM rendering pipeline (`web-rendering.md` §1): frontmatter
//! strip, comment strip, Mermaid extraction, wikilink/embed extraction,
//! highlight and inline tag rewriting, triple-colon fence and callout
//! resolution, then general Markdown, in that stage order.
//!
//! [`compile`] is the pure half: it extracts every occurrence needing an
//! MCP round trip (wikilink/embed target, Mermaid source) into a
//! placeholder token without making one itself, so it is unit-tested
//! without any MCP dependency (`testing.md`). [`render`] (async) resolves
//! them against a live [`McpClient`] and substitutes the final HTML.
//! Mermaid and wikilink/embed extraction run once, globally, before
//! triple-colon fence and callout extraction, so a fence or callout body
//! may itself contain a resolved wikilink or embed (their placeholder
//! tokens simply land inside that block's own inner HTML, substituted in
//! the same final pass as every top-level occurrence); a fence body may
//! additionally contain a Mermaid diagram, since its lines carry no extra
//! prefix. A callout body's lines each carry a leading `>` (Markdown
//! blockquote continuation), which the global Mermaid scan does not
//! dedent before looking for a fence-open line, so a Mermaid diagram
//! inside a callout is not supported in v1: a documented scope limit, not
//! an oversight. Fences and callouts may nest inside each other and inside
//! themselves, up to [`MAX_FENCE_NESTING_DEPTH`]: see
//! [`render_fenced_and_callout_html`]'s own doc for how that recursion
//! stays clear of the global wikilink/Mermaid placeholder space.

use std::collections::HashMap;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use serde::Deserialize;

use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::diagnostics::{self, Diagnostic};
use crate::rendering::wikilinks::{LinkOccurrence, LinkSyntax};
use crate::rendering::{block_ids, callouts, comments, fences, frontmatter, highlight, tags, wikilinks};

/// An embed nests at most one level deep: a target that is itself an embed
/// of a third file renders as a plain link at that second level, never a
/// second recursive inline.
const MAX_EMBED_DEPTH: u8 = 1;

/// Converts already OFM-extension-resolved text (frontmatter, fences,
/// callouts, wikilinks, and Mermaid already reduced to raw HTML or
/// placeholder tokens) to HTML via CommonMark/GFM (`web-rendering.md` §1
/// stage 5's non-OFM-specific remainder): tables, strikethrough,
/// footnotes, task lists, and smart punctuation enabled, matching OFM's
/// own superset.
#[must_use]
pub fn html_from_commonmark(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    // `$inline$`/`$$display$$` math (the `obsidian-markdown` skill's own
    // documented syntax): `pulldown-cmark` renders each as a semantic
    // `<span class="math math-inline">`/`<span class="math math-display">`
    // with no custom event handling needed. This marks the LaTeX source up
    // correctly rather than leaking it as literal `$...$` text; an actual
    // client-side typesetter (KaTeX/MathJax) is a separate, not-yet-shipped
    // concern (`A-055`).
    options.insert(Options::ENABLE_MATH);
    let parser = Parser::new_ext(source, options);
    let collected: Vec<Event<'_>> = parser.collect();
    let events = assign_heading_ids(&collected);
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

/// Assigns a stable, Obsidian-style slug `id` to every heading (this
/// pipeline never turns on `ENABLE_HEADING_ATTRIBUTES`, so an author's own
/// explicit `{#id}` override is not supported: every heading always gets
/// an auto-generated one), so a `[[Note#Heading]]` link has something to
/// land on ([`append_fragment`]). Two headings that slugify to the same
/// text on one page disambiguate with a numeric suffix, matching the same
/// discipline GitHub's own heading anchors use.
fn assign_heading_ids<'a>(events: &[Event<'a>]) -> Vec<Event<'a>> {
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        let Event::Start(Tag::Heading {
            level, classes, attrs, ..
        }) = &events[i]
        else {
            out.push(events[i].clone());
            i += 1;
            continue;
        };
        let (level, classes, attrs) = (*level, classes.clone(), attrs.clone());
        let mut text = String::new();
        let mut j = i + 1;
        while j < events.len() {
            match &events[j] {
                Event::End(TagEnd::Heading(_)) => break,
                Event::Text(t) | Event::Code(t) => text.push_str(t),
                _ => {}
            }
            j += 1;
        }
        out.push(Event::Start(Tag::Heading {
            level,
            id: Some(unique_slug(&text, &mut used).into()),
            classes,
            attrs,
        }));
        i += 1;
    }
    out
}

fn unique_slug(text: &str, used: &mut HashMap<String, u32>) -> String {
    let base = slugify(text);
    let base = if base.is_empty() { "section".to_owned() } else { base };
    match used.get_mut(&base) {
        None => {
            used.insert(base.clone(), 0);
            base
        }
        Some(count) => {
            *count += 1;
            format!("{base}-{count}")
        }
    }
}

/// Obsidian-style heading-anchor slug: lowercased, every run of
/// non-alphanumeric characters collapsed to a single hyphen, no leading or
/// trailing hyphen. Shared by [`assign_heading_ids`] (a real heading's own
/// id) and [`append_fragment`] (a `[[Note#Heading]]` link's own `#`
/// fragment), so the two agree as long as the link's own written heading
/// text matches what the target note actually has (the author's own
/// responsibility: verifying it would mean resolving and parsing the
/// target note's own content, a materially bigger round trip this stays
/// without).
fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut last_was_hyphen = true;
    for c in text.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Appends `occurrence`'s own `#heading`/`#^block-id` fragment to an
/// already-resolved `href`, when present. A block reference uses its own
/// literal id verbatim, matching [`block_ids`]'s own anchor id exactly (no
/// slugification: a block id is already an author-chosen token, not prose
/// to normalise); a heading reference is [`slugify`]d the same way a real
/// heading's own id is.
fn append_fragment(href: &str, occurrence: &LinkOccurrence) -> String {
    if let Some(block) = &occurrence.block {
        format!("{href}#{block}")
    } else if let Some(heading) = &occurrence.heading {
        format!("{href}#{}", slugify(heading))
    } else {
        href.to_owned()
    }
}

#[must_use]
fn mermaid_placeholder(index: usize) -> String {
    format!("\u{E000}MERMAID:{index}\u{E000}")
}

/// Extracts every top-level Mermaid-fenced block (an info string of
/// exactly `mermaid` on an opening triple-backtick fence) from `text`,
/// replacing each with its placeholder. A differently-fenced block's body
/// is consumed as opaque content, so a literal Mermaid-fence line inside it
/// (for example, documentation about Mermaid syntax) is never mistaken for
/// a real fence, mirroring `contextos-mcp`'s own `extract_mermaid_fence`.
fn extract_mermaid(text: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut sources = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            let is_mermaid = info.trim().eq_ignore_ascii_case("mermaid");
            if let Some(close) = lines[i + 1..].iter().position(|l| l.trim_start().starts_with("```")) {
                let body_start = i + 1;
                let body_end = body_start + close;
                if is_mermaid {
                    let index = sources.len();
                    sources.push(lines[body_start..body_end].join("\n"));
                    if out_lines.last().is_some_and(|l: &String| !l.is_empty()) {
                        out_lines.push(String::new());
                    }
                    out_lines.push(mermaid_placeholder(index));
                    out_lines.push(String::new());
                } else {
                    out_lines.extend(lines[i..=body_end].iter().map(|l| (*l).to_owned()));
                }
                i = body_end + 1;
                continue;
            }
        }
        out_lines.push(lines[i].to_owned());
        i += 1;
    }
    (out_lines.join("\n"), sources)
}

/// Replaces a block-level placeholder (a fence, callout, embed, or Mermaid
/// token that landed on its own paragraph) with `replacement`. Falls back
/// to a bare token replace when the token was not wrapped in `<p>...</p>`
/// (a defensive fallback; every extractor emits its placeholder on a
/// blank-line-delimited line, so this should not occur in practice).
fn replace_block_placeholder(html: &str, token: &str, replacement: &str) -> String {
    let wrapped = format!("<p>{token}</p>");
    if html.contains(&wrapped) {
        html.replacen(&wrapped, replacement, 1)
    } else {
        html.replacen(token, replacement, 1)
    }
}

/// The pure "compile" result: HTML still carrying `LINK`/`EMBED`/`MERMAID`
/// placeholder tokens, plus what each needs resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub html: String,
    /// Wikilink/embed occurrences in original scan order; index `i`
    /// corresponds to `wikilinks::placeholder(i)` or
    /// `wikilinks::embed_placeholder(i)` depending on `occurrences[i].syntax`.
    pub occurrences: Vec<LinkOccurrence>,
    /// Mermaid sources in original scan order; index `i` corresponds to
    /// `mermaid_placeholder(i)`.
    pub mermaid_sources: Vec<String>,
}

/// Runs every MCP-free stage of the pipeline (`web-rendering.md` §1 stages
/// 1 to 5, minus Mermaid's own render call).
#[must_use]
pub fn compile(raw_source: &str) -> Compiled {
    let body = frontmatter::strip(raw_source);
    let body = comments::strip(body);
    let (body, mermaid_sources) = extract_mermaid(&body);
    let wiki = wikilinks::extract(&body);
    let highlighted = highlight::apply(&wiki.text);
    let tagged = tags::apply(&highlighted);
    let anchored = block_ids::apply(&tagged);
    let html = render_fenced_and_callout_html(&anchored, 0);

    Compiled {
        html,
        occurrences: wiki.occurrences,
        mermaid_sources,
    }
}

/// A fence or callout nested inside another renders no deeper than this:
/// `standards/markdown-fence-conventions.md` itself recommends keeping
/// nested fences shallow, and this bound exists to make pathological or
/// runaway input (not a realistic authored document) fail safely — falling
/// back to the block's raw text rendered as plain Markdown, never an
/// infinite recursion — mirroring wikilink embeds' own `MAX_EMBED_DEPTH`.
const MAX_FENCE_NESTING_DEPTH: u8 = 4;

/// Resolves every triple-colon fence and Obsidian callout in `text` to
/// final HTML, recursing into each block's own inner text so a fence or
/// callout nested inside another (`fences.rs`'s and `callouts.rs`'s own
/// module docs) renders as its own real, typed container rather than
/// literal `:::name`/`> [!type]` text (`web-rendering.md` §1 stages 2 and
/// 4). Wikilink, embed, Mermaid, highlight, and tag placeholders are
/// untouched here: those already ran as single global passes over the
/// whole document before `compile` ever calls this, so their placeholder
/// tokens are already globally unique and simply ride along inertly inside
/// whatever fence/callout body they land in, resolved later by `render`'s
/// own top-level substitution pass over the fully-assembled document. Each
/// recursive call is fully self-contained (its own local fence/callout
/// blocks, fully substituted before it returns), so nested calls never
/// need a shared placeholder-index space the way that would risk a
/// collision.
fn render_fenced_and_callout_html(text: &str, depth: u8) -> String {
    let fence_result = fences::extract(text);
    let callout_result = callouts::extract(&fence_result.text);
    let mut html = html_from_commonmark(&callout_result.text);

    for (index, block) in callout_result.blocks.iter().enumerate() {
        let body_html = if depth >= MAX_FENCE_NESTING_DEPTH {
            html_from_commonmark(&block.body)
        } else {
            render_fenced_and_callout_html(&block.body, depth + 1)
        };
        let rendered = callouts::render(&block.open, &body_html);
        html = replace_block_placeholder(&html, &callouts::placeholder(index), &rendered);
    }
    for (index, block) in fence_result.blocks.iter().enumerate() {
        let inner_html = if depth >= MAX_FENCE_NESTING_DEPTH {
            html_from_commonmark(&block.inner)
        } else {
            render_fenced_and_callout_html(&block.inner, depth + 1)
        };
        let rendered = fences::render(&block.open, &inner_html);
        html = replace_block_placeholder(&html, &fences::placeholder(index), &rendered);
    }
    html
}

/// The final rendered page body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMarkdown {
    pub html: String,
}

/// Resolves every placeholder [`compile`] left behind against a live
/// [`McpClient`] and returns the final HTML: a pure function of the tool
/// calls' own results, so identical vault state renders byte-identical
/// output.
pub async fn render(mcp: &McpClient, vault_name: &str, raw_source: &str, embed_depth: u8) -> RenderedMarkdown {
    let compiled = compile(raw_source);
    let mut html = compiled.html;

    for (index, source) in compiled.mermaid_sources.iter().enumerate() {
        let rendered = render_mermaid_block(mcp, source).await;
        html = replace_block_placeholder(&html, &mermaid_placeholder(index), &rendered);
    }

    // A single render pass resolves each distinct wikilink/embed target at
    // most once: a note repeating the same target many times (a heavily
    // cross-referenced index page) would otherwise cost one MCP round trip
    // per occurrence rather than per distinct target, the dominant cost
    // this crate's rendering performance budget actually measures.
    let mut href_cache: HashMap<String, Option<String>> = HashMap::new();
    for (index, occurrence) in compiled.occurrences.iter().enumerate() {
        match occurrence.syntax {
            LinkSyntax::Link => {
                let href = resolve_href_cached(mcp, vault_name, &occurrence.target, &mut href_cache).await;
                let rendered = match href {
                    Some(h) => wikilinks::render_link(occurrence, &append_fragment(&h, occurrence)),
                    None => wikilinks::render_dead_link(occurrence),
                };
                html = html.replacen(&wikilinks::placeholder(index), &rendered, 1);
            }
            LinkSyntax::Embed => {
                let rendered = render_embed_occurrence(mcp, vault_name, occurrence, embed_depth, &mut href_cache).await;
                html = replace_block_placeholder(&html, &wikilinks::embed_placeholder(index), &rendered);
            }
        }
    }

    RenderedMarkdown { html }
}

async fn resolve_href_cached(
    mcp: &McpClient,
    vault_name: &str,
    target: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(cached) = cache.get(target) {
        return cached.clone();
    }
    let resolved = resolve_href(mcp, vault_name, target).await;
    cache.insert(target.to_owned(), resolved.clone());
    resolved
}

/// Extensions `render_embed_occurrence` renders as media (`<img>`/`<audio>`)
/// or a plain link (PDF) rather than attempting `fs_read_text_file` on
/// them: every one is a binary format that read would always fail on
/// (`FsError::Binary`), producing a dead link for a target that genuinely
/// resolved, not a missing one.
///
/// Matches the browser's own native decoder support, the same
/// "browser-renderable" set `web-routes.md`'s generic-file dispatch
/// describes, and the `obsidian-markdown` skill's own `EMBEDS.md`
/// ("Embed Images", "Embed Audio", "Embed PDF").
const IMAGE_EXTENSIONS: [&str; 9] = ["jpg", "jpeg", "png", "gif", "webp", "svg", "bmp", "avif", "ico"];
const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "ogg", "wav", "m4a", "flac", "aac", "opus", "webm"];
const PDF_EXTENSIONS: [&str; 1] = ["pdf"];
const BASE_EXTENSIONS: [&str; 1] = ["base"];

fn extension_matches(path: &str, extensions: &[&str]) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, ext)| extensions.contains(&ext.to_ascii_lowercase().as_str()))
}

fn is_image_path(path: &str) -> bool {
    extension_matches(path, &IMAGE_EXTENSIONS)
}

fn is_audio_path(path: &str) -> bool {
    extension_matches(path, &AUDIO_EXTENSIONS)
}

fn is_pdf_path(path: &str) -> bool {
    extension_matches(path, &PDF_EXTENSIONS)
}

fn is_base_path(path: &str) -> bool {
    extension_matches(path, &BASE_EXTENSIONS)
}

/// Parses an image embed's own `|WIDTH` or `|WIDTHxHEIGHT` size hint
/// (`EMBEDS.md`: `![[image.png|300]]`, `![[image.png|640x480]]`).
/// Obsidian overloads the pipe segment: "display text" for a link, "size"
/// for an image embed. Malformed or missing sizing (anything that does not
/// parse as one or two positive integers) is simply no hint at all, never
/// a render failure over a decorative sizing detail.
fn parse_image_size(hint: &str) -> Option<(u32, Option<u32>)> {
    let hint = hint.trim();
    if let Some((width, height)) = hint.split_once('x') {
        return Some((width.trim().parse().ok()?, Some(height.trim().parse().ok()?)));
    }
    Some((hint.parse().ok()?, None))
}

/// Renders an image embed (`![[photo.jpg]]`) as an `<img>` pointing at the
/// target's own vault content route (`crate::routes::vault::render_other_file`
/// already serves it as raw bytes with its real content type): unlike a
/// note embed, an image is never read as text or recursively rendered.
fn render_image_embed(occurrence: &LinkOccurrence, href: &str) -> String {
    let size_attrs = match occurrence.display.as_deref().and_then(parse_image_size) {
        Some((width, Some(height))) => format!(" width=\"{width}\" height=\"{height}\""),
        Some((width, None)) => format!(" width=\"{width}\""),
        None => String::new(),
    };
    format!(
        "<img class=\"embed-image\" src=\"{href}\" alt=\"{alt}\"{size_attrs}>",
        href = crate::rendering::escape_html(href),
        alt = crate::rendering::escape_html(&occurrence.target)
    )
}

/// Renders an audio embed (`![[audio.mp3]]`) as a native `<audio controls>`
/// element pointing at the same content route an image embed uses.
fn render_audio_embed(occurrence: &LinkOccurrence, href: &str) -> String {
    format!(
        "<audio class=\"embed-audio\" controls src=\"{href}\">{fallback}</audio>",
        href = crate::rendering::escape_html(href),
        fallback = crate::rendering::escape_html(&occurrence.target)
    )
}

async fn render_embed_occurrence(
    mcp: &McpClient,
    vault_name: &str,
    occurrence: &LinkOccurrence,
    embed_depth: u8,
    href_cache: &mut HashMap<String, Option<String>>,
) -> String {
    let Some(candidate) = resolve_href_cached(mcp, vault_name, &occurrence.target, href_cache).await else {
        return wikilinks::render_dead_link(occurrence);
    };
    if is_image_path(&candidate) {
        return render_image_embed(occurrence, &candidate);
    }
    if is_audio_path(&candidate) {
        return render_audio_embed(occurrence, &candidate);
    }
    if is_pdf_path(&candidate) {
        // A PDF embed renders as a plain link in v1: a full inline preview
        // (`EMBEDS.md`'s `#page=`/`#height=` fragments) needs a bundled PDF
        // viewer this crate does not yet ship, tracked separately
        // (`A-055`); a link is still strictly better than the dead link a
        // PDF's binary content would otherwise produce.
        return wikilinks::render_link(occurrence, &candidate);
    }
    let relative_path = candidate
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix(vault_name))
        .and_then(|p| p.strip_prefix('/'))
        .unwrap_or(&candidate)
        .to_owned();
    if is_base_path(&candidate) {
        // A `.base` file is a view definition, never text to read or
        // recursively render (`web-rendering.md` §2): reuses the identical
        // `base::render_view` the `.base` file's own route already calls.
        // The occurrence's own `heading` fragment doubles as the view name
        // here, `EMBEDS.md`'s `#View Name` convention.
        return render_base_embed(mcp, vault_name, occurrence, &relative_path).await;
    }
    if embed_depth >= MAX_EMBED_DEPTH {
        // A doubly-embedded file renders as a plain link at the second
        // level, never a second recursive inline.
        return wikilinks::render_link(occurrence, &append_fragment(&candidate, occurrence));
    }
    let path = format!("{vault_name}://{relative_path}");
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), serde_json::Value::String(path.clone()));
    let Ok(result) = mcp.call_tool("fs_read_text_file".to_owned(), args).await else {
        return wikilinks::render_dead_link(occurrence);
    };
    if result.is_error == Some(true) {
        return wikilinks::render_dead_link(occurrence);
    }
    let Ok(read) = result.into_typed::<ReadTextResult>() else {
        return wikilinks::render_dead_link(occurrence);
    };
    let content = if read.truncated {
        // `fs_read_text_file` caps out at 5 KiB; `fs_attach_file` returns
        // full content for a file of any size, so a truncated embed
        // retries through it rather than silently inlining a partial note.
        // A retry failure falls back to the truncated content already in
        // hand rather than dropping the embed entirely.
        fetch_full_text(mcp, path).await.ok().flatten().unwrap_or(read.content)
    } else {
        read.content
    };
    // `![[Note#^block-id]]` inlines just the referenced block
    // (`EMBEDS.md`'s "Embed Lists"), not the whole target note; a
    // heading-only fragment (`![[Note#Heading]]`) does not narrow the
    // embed in v1 and still inlines the full note, a smaller committed
    // scope than partial block extraction (`A-055`).
    let source = match &occurrence.block {
        Some(block_id) => block_ids::extract_block_by_id(&content, block_id).unwrap_or(content),
        None => content,
    };
    let rendered = Box::pin(render(mcp, vault_name, &source, embed_depth + 1)).await;
    format!(
        "<div class=\"embed-block\"><div class=\"embed-label\">{label}</div>{body}</div>",
        label = crate::rendering::escape_html(&occurrence.target),
        body = rendered.html
    )
}

async fn render_base_embed(
    mcp: &McpClient,
    vault_name: &str,
    occurrence: &LinkOccurrence,
    relative_path: &str,
) -> String {
    match crate::rendering::base::render_view(mcp, vault_name, relative_path, occurrence.heading.as_deref()).await {
        Ok(fragment) => format!(
            "<div class=\"embed-block\"><div class=\"embed-label\">{label}</div>{fragment}</div>",
            label = crate::rendering::escape_html(&occurrence.target)
        ),
        Err(McpCallError::Unreachable { .. }) => wikilinks::render_dead_link(occurrence),
    }
}

/// Re-reads `vault_path` via `fs_attach_file` (full content for a file of
/// any size, unlike `fs_read_text_file`'s 5 KiB cap): the completeness
/// retry for a text read [`ReadTextResult::truncated`] flagged. `None` for
/// a non-text attachment (should not happen for a path that just read as
/// truncated text, but fails closed rather than misinterpreting binary
/// content as a string) or a target that no longer exists.
async fn fetch_full_text(mcp: &McpClient, vault_path: String) -> Result<Option<String>, McpCallError> {
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), serde_json::Value::String(vault_path));
    let result = mcp.call_tool("fs_attach_file".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    for block in &result.content {
        let rmcp::model::ContentBlock::Resource(embedded) = block else {
            continue;
        };
        if let rmcp::model::ResourceContents::TextResourceContents { text, .. } = &embedded.resource {
            return Ok(Some(text.clone()));
        }
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct ReadTextResult {
    content: String,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

/// Resolves a wikilink target to its `/{{vault_name}}/{{relative-path}}`
/// route: an exact relative-path match (with `.md` assumed when the target
/// carries no extension) is tried first, falling back to a basename search
/// across the vault when exactly one candidate matches. `None` means dead.
async fn resolve_href(mcp: &McpClient, vault_name: &str, target: &str) -> Option<String> {
    let candidate = normalise_target(target);
    let exact = format!("{vault_name}://{candidate}");
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), serde_json::Value::String(exact));
    if let Ok(result) = mcp.call_tool("fs_get_file_info".to_owned(), args).await
        && result.is_error != Some(true)
    {
        return Some(format!("/{vault_name}/{candidate}"));
    }

    let basename = candidate.rsplit('/').next().unwrap_or(&candidate);
    let mut search_args = serde_json::Map::new();
    search_args.insert(
        "path".to_owned(),
        serde_json::Value::String(format!("{vault_name}://.")),
    );
    search_args.insert(
        "pattern".to_owned(),
        serde_json::Value::String(format!("**/{basename}")),
    );
    let result = mcp.call_tool("fs_search_files".to_owned(), search_args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let found = result.into_typed::<SearchFilesResult>().ok()?;
    if found.paths.len() == 1 {
        let relative = found.paths[0].trim_start_matches('/');
        Some(format!("/{vault_name}/{relative}"))
    } else {
        None
    }
}

fn normalise_target(target: &str) -> String {
    let last_segment = target.rsplit('/').next().unwrap_or(target);
    if last_segment.contains('.') {
        target.to_owned()
    } else {
        format!("{target}.md")
    }
}

/// Renders a standalone `.mermaid` file's own full content as one diagram
/// (`web-routes.md` §2: "standalone file, not a fenced block"), unlike a
/// ` ```mermaid ` fenced block within a `.md` note.
pub async fn render_mermaid_source(mcp: &McpClient, source: &str) -> String {
    render_mermaid_block(mcp, source).await
}

async fn render_mermaid_block(mcp: &McpClient, source: &str) -> String {
    let mut args = serde_json::Map::new();
    args.insert("source".to_owned(), serde_json::Value::String(source.to_owned()));
    let Ok(result) = mcp.call_tool("mermaid_render".to_owned(), args).await else {
        return diagnostics::render_diagnostic_panel(&[Diagnostic {
            code: "mcp/unreachable".to_owned(),
            path: String::new(),
            message: "Mermaid rendering is unavailable.".to_owned(),
        }]);
    };
    if let Some(svg) = extract_svg(&result) {
        return format!("<div class=\"mermaid-diagram\">{svg}</div>");
    }
    let diagnostics_result = result.into_typed::<MermaidDiagnosticsResult>().map_or_else(
        |_| {
            vec![Diagnostic {
                code: "mermaid/render-failed".to_owned(),
                path: String::new(),
                message: "The diagram could not be rendered.".to_owned(),
            }]
        },
        |r| r.diagnostics,
    );
    diagnostics::render_diagnostic_panel(&diagnostics_result)
}

fn extract_svg(result: &rmcp::model::CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Resource(embedded) => match &embedded.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                mime_type: Some(mime),
                text,
                ..
            } if mime == "image/svg+xml" => Some(text.clone()),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
struct MermaidDiagnosticsResult {
    diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
#[path = "markdown_test.rs"]
mod tests;
