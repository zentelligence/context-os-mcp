use thiserror::Error;

/// One outgoing Obsidian wikilink or embedded wikilink.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsidianLink {
    pub target: String,
    pub display: Option<String>,
    pub heading: Option<String>,
    pub block: Option<String>,
    pub embed: bool,
}

#[derive(Clone, Copy, Debug)]
struct LinkInput<'a> {
    raw: &'a str,
    line: usize,
    embed: bool,
}

impl TryFrom<LinkInput<'_>> for ObsidianLink {
    type Error = MarkdownError;

    fn try_from(value: LinkInput<'_>) -> Result<Self, Self::Error> {
        let (destination, display) = match value.raw.split_once('|') {
            Some((destination, display)) => (destination, Some(display.trim().to_owned())),
            None => (value.raw, None),
        };
        let (target, heading, block) = match destination.split_once('#') {
            Some((target, anchor)) if anchor.starts_with('^') => {
                (target.trim(), None, Some(anchor.trim_start_matches('^').to_owned()))
            }
            Some((target, heading)) => (target.trim(), Some(heading.to_owned()), None),
            None => (destination.trim(), None, None),
        };
        let has_anchor = heading.as_ref().is_some_and(|anchor| !anchor.is_empty())
            || block.as_ref().is_some_and(|anchor| !anchor.is_empty());
        if target.is_empty() && !has_anchor {
            return Err(MarkdownError::EmptyLinkTarget { line: value.line });
        }

        Ok(Self {
            target: target.to_owned(),
            display,
            heading,
            block,
            embed: value.embed,
        })
    }
}

/// Outgoing links extracted from an Obsidian Markdown source document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkCollection {
    outgoing: Vec<ObsidianLink>,
}

impl TryFrom<&str> for LinkCollection {
    type Error = MarkdownError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let mut outgoing = Vec::new();
        let mut in_fence = false;
        let mut in_comment = false;

        for (line_index, line) in source.split_inclusive('\n').enumerate() {
            let line_number = line_index.saturating_add(1);
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            scan_line(line, line_number, &mut in_comment, &mut outgoing)?;
        }

        Ok(Self { outgoing })
    }
}

impl TryFrom<String> for LinkCollection {
    type Error = MarkdownError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl LinkCollection {
    /// Returns outgoing links in source order.
    #[must_use]
    pub fn outgoing(&self) -> &[ObsidianLink] {
        &self.outgoing
    }
}

/// Obsidian Markdown proven to contain well-formed links, embeds, and callouts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedMarkdown {
    links: LinkCollection,
}

impl TryFrom<&str> for ValidatedMarkdown {
    type Error = MarkdownError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let links = LinkCollection::try_from(source)?;
        let mut in_fence = false;
        for (line_index, line) in source.lines().enumerate() {
            let line_number = line_index.saturating_add(1);
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if !in_fence {
                validate_callout(line, line_number)?;
            }
        }
        Ok(Self { links })
    }
}

impl TryFrom<String> for ValidatedMarkdown {
    type Error = MarkdownError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl ValidatedMarkdown {
    /// Returns the outgoing links discovered during validation.
    #[must_use]
    pub const fn links(&self) -> &LinkCollection {
        &self.links
    }
}

fn validate_callout(line: &str, line_number: usize) -> Result<(), MarkdownError> {
    let mut remaining = line.trim_start();
    while let Some(nested) = remaining.strip_prefix('>') {
        remaining = nested.trim_start();
    }
    let Some(declaration) = remaining.strip_prefix("[!") else {
        return Ok(());
    };
    let Some(closing) = declaration.find(']') else {
        return Err(MarkdownError::InvalidCallout { line: line_number });
    };
    let callout_type = &declaration[..closing];
    if callout_type.is_empty()
        || !callout_type
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(MarkdownError::InvalidCallout { line: line_number });
    }
    Ok(())
}

fn scan_line(
    line: &str,
    line_number: usize,
    in_comment: &mut bool,
    outgoing: &mut Vec<ObsidianLink>,
) -> Result<(), MarkdownError> {
    let mut cursor = 0_usize;
    let mut inline_ticks = None;
    while cursor < line.len() {
        let remaining = &line[cursor..];
        if remaining.starts_with('`') {
            let run = remaining.chars().take_while(|character| *character == '`').count();
            inline_ticks = match inline_ticks {
                Some(opening) if opening == run => None,
                Some(opening) => Some(opening),
                None => Some(run),
            };
            cursor = cursor.saturating_add(run);
            continue;
        }
        if inline_ticks.is_none() && remaining.starts_with("%%") {
            *in_comment = !*in_comment;
            cursor = cursor.saturating_add(2);
            continue;
        }
        if inline_ticks.is_none() && !*in_comment && remaining.starts_with('\\') {
            cursor = cursor.saturating_add(1);
            if let Some(character) = line[cursor..].chars().next() {
                cursor = cursor.saturating_add(character.len_utf8());
            }
            continue;
        }
        if inline_ticks.is_none() && !*in_comment {
            let (embed, opening_length) = if remaining.starts_with("![[") {
                (true, 3_usize)
            } else if remaining.starts_with("[[") {
                (false, 2_usize)
            } else {
                let Some(character) = remaining.chars().next() else {
                    break;
                };
                cursor = cursor.saturating_add(character.len_utf8());
                continue;
            };
            let content_start = cursor.saturating_add(opening_length);
            let Some(relative_end) = line[content_start..].find("]]") else {
                return Err(MarkdownError::UnclosedLink {
                    line: line_number,
                    embed,
                });
            };
            let content_end = content_start.saturating_add(relative_end);
            outgoing.push(ObsidianLink::try_from(LinkInput {
                raw: &line[content_start..content_end],
                line: line_number,
                embed,
            })?);
            cursor = content_end.saturating_add(2);
            continue;
        }

        let Some(character) = remaining.chars().next() else {
            break;
        };
        cursor = cursor.saturating_add(character.len_utf8());
    }
    Ok(())
}

/// Typed Obsidian Markdown validation failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MarkdownError {
    #[error("wikilink on line {line} has no closing brackets")]
    UnclosedLink { line: usize, embed: bool },
    #[error("wikilink on line {line} has no target, heading, or block")]
    EmptyLinkTarget { line: usize },
    #[error("callout declaration on line {line} is malformed")]
    InvalidCallout { line: usize },
}

impl MarkdownError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/markdown"
    }

    /// Returns an actionable correction for invalid Obsidian syntax.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the Obsidian wikilink or embed syntax and retry."
    }
}
