//! Markdown lowered to Glory's finite semantic vocabulary; no HTML injection.
use std::cell::Cell;

use glory::prelude::*;
use pulldown_cmark::{CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Marks {
    strong: bool,
    emphasis: bool,
    strike: bool,
    code: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    text: String,
    marks: Marks,
    target: Option<String>,
    image: bool,
}
#[derive(Debug)]
enum Block {
    Paragraph {
        runs: Vec<Run>,
        heading: Option<u8>,
        quote: bool,
    },
    Code {
        source: String,
        language: String,
    },
    Table {
        header: Vec<Vec<Run>>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
}
#[derive(Default)]
struct TableBuffer {
    header: Vec<Vec<Run>>,
    rows: Vec<Vec<Vec<Run>>>,
    row: Vec<Vec<Run>>,
}

fn safe_target(value: &str) -> bool {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value.starts_with("//")
    {
        return false;
    }
    if let Some((scheme, rest)) = value.split_once(':') {
        !rest.is_empty()
            && ["https", "http", "mailto"]
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    } else {
        true
    }
}

fn parse(content: &str) -> Vec<Block> {
    let mut output = Vec::new();
    let mut runs = Vec::<Run>::new();
    let mut marks = Marks::default();
    let mut strong = 0_usize;
    let mut emphasis = 0_usize;
    let mut strike = 0_usize;
    let mut heading = None;
    let mut quote = 0_usize;
    let mut target = None;
    let mut image = false;
    let mut lists = Vec::<Option<u64>>::new();
    let mut table = None::<TableBuffer>;
    let mut code = None::<(String, String)>;
    let flush =
        |output: &mut Vec<Block>, runs: &mut Vec<Run>, heading: Option<u8>, quote: usize| {
            if !runs.is_empty() {
                output.push(Block::Paragraph {
                    runs: std::mem::take(runs),
                    heading,
                    quote: quote > 0,
                });
            }
        };
    for event in Parser::new_ext(
        content,
        Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            MdEvent::Start(Tag::Heading { level, .. }) => {
                flush(&mut output, &mut runs, heading, quote);
                heading = Some(level as u8);
            }
            MdEvent::End(TagEnd::Heading(_)) => {
                flush(&mut output, &mut runs, heading, quote);
                heading = None;
            }
            MdEvent::Start(Tag::BlockQuote(_)) => {
                flush(&mut output, &mut runs, heading, quote);
                quote += 1;
            }
            MdEvent::End(TagEnd::BlockQuote(_)) => {
                flush(&mut output, &mut runs, heading, quote);
                quote = quote.saturating_sub(1);
            }
            MdEvent::Start(Tag::Strong) => {
                strong += 1;
                marks.strong = true;
            }
            MdEvent::End(TagEnd::Strong) => {
                strong = strong.saturating_sub(1);
                marks.strong = strong > 0;
            }
            MdEvent::Start(Tag::Emphasis) => {
                emphasis += 1;
                marks.emphasis = true;
            }
            MdEvent::End(TagEnd::Emphasis) => {
                emphasis = emphasis.saturating_sub(1);
                marks.emphasis = emphasis > 0;
            }
            MdEvent::Start(Tag::Strikethrough) => {
                strike += 1;
                marks.strike = true;
            }
            MdEvent::End(TagEnd::Strikethrough) => {
                strike = strike.saturating_sub(1);
                marks.strike = strike > 0;
            }
            MdEvent::Start(Tag::Link { dest_url, .. }) => target = Some(dest_url.into_string()),
            MdEvent::End(TagEnd::Link) => target = None,
            MdEvent::Start(Tag::Image { dest_url, .. }) => {
                target = Some(dest_url.into_string());
                image = true;
            }
            MdEvent::End(TagEnd::Image) => {
                target = None;
                image = false;
            }
            MdEvent::Start(Tag::CodeBlock(kind)) => {
                flush(&mut output, &mut runs, heading, quote);
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((String::new(), language));
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                if let Some((source, language)) = code.take() {
                    output.push(Block::Code { source, language });
                }
            }
            MdEvent::Start(Tag::List(start)) => {
                flush(&mut output, &mut runs, heading, quote);
                lists.push(start);
            }
            MdEvent::End(TagEnd::List(_)) => {
                flush(&mut output, &mut runs, heading, quote);
                lists.pop();
            }
            MdEvent::Start(Tag::Item) => {
                flush(&mut output, &mut runs, heading, quote);
                let indent = "  ".repeat(lists.len().saturating_sub(1));
                let marker = if let Some(Some(index)) = lists.last_mut() {
                    let value = format!("{indent}{index}. ");
                    *index = index.saturating_add(1);
                    value
                } else {
                    format!("{indent}- ")
                };
                runs.push(Run {
                    text: marker,
                    marks: Marks::default(),
                    target: None,
                    image: false,
                });
            }
            MdEvent::End(TagEnd::Item | TagEnd::Paragraph) => {
                if table.is_none() {
                    flush(&mut output, &mut runs, heading, quote);
                }
            }
            MdEvent::Start(Tag::Table(_)) => {
                flush(&mut output, &mut runs, heading, quote);
                table = Some(TableBuffer::default());
            }
            MdEvent::End(TagEnd::TableCell) => {
                if let Some(table) = &mut table {
                    table.row.push(std::mem::take(&mut runs));
                }
            }
            MdEvent::End(TagEnd::TableHead) => {
                if let Some(table) = &mut table {
                    table.header = std::mem::take(&mut table.row);
                }
            }
            MdEvent::End(TagEnd::TableRow) => {
                if let Some(table) = &mut table {
                    table.rows.push(std::mem::take(&mut table.row));
                }
            }
            MdEvent::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    output.push(Block::Table {
                        header: table.header,
                        rows: table.rows,
                    });
                }
            }
            MdEvent::Start(Tag::FootnoteDefinition(name)) => {
                flush(&mut output, &mut runs, heading, quote);
                runs.push(Run {
                    text: format!("[{name}]: "),
                    marks: Marks::default(),
                    target: None,
                    image: false,
                });
            }
            MdEvent::End(TagEnd::FootnoteDefinition) => {
                flush(&mut output, &mut runs, heading, quote)
            }
            MdEvent::FootnoteReference(name) => runs.push(Run {
                text: format!("[{name}]"),
                marks: Marks::default(),
                target: None,
                image: false,
            }),
            MdEvent::Text(value) | MdEvent::Html(value) | MdEvent::InlineHtml(value) => {
                if let Some((source, _)) = &mut code {
                    source.push_str(&value);
                } else {
                    runs.push(Run {
                        text: value.into_string(),
                        marks,
                        target: target.clone(),
                        image,
                    });
                }
            }
            MdEvent::Code(value) => runs.push(Run {
                text: value.into_string(),
                marks: Marks {
                    code: true,
                    ..marks
                },
                target: target.clone(),
                image: false,
            }),
            MdEvent::SoftBreak | MdEvent::HardBreak => runs.push(Run {
                text: "\n".into(),
                marks,
                target: target.clone(),
                image: false,
            }),
            MdEvent::TaskListMarker(checked) => runs.push(Run {
                text: if checked { "[x] " } else { "[ ] " }.into(),
                marks: Marks::default(),
                target: None,
                image: false,
            }),
            MdEvent::Rule => {
                flush(&mut output, &mut runs, heading, quote);
                output.push(Block::Rule);
            }
            _ => {}
        }
    }
    flush(&mut output, &mut runs, heading, quote);
    output
}

fn run_widget(run: Run) -> Box<dyn Widget> {
    let mut class = "markdown-run".to_owned();
    if run.marks.strong {
        class.push_str(" markdown-strong");
    }
    if run.marks.emphasis {
        class.push_str(" markdown-emphasis");
    }
    if run.marks.strike {
        class.push_str(" markdown-strike");
    }
    if run.marks.code {
        class.push_str(" markdown-inline-code");
    }
    if let Some(target) = run.target.filter(|target| safe_target(target)) {
        if run.image && !target.starts_with("mailto:") && !target.starts_with('#') {
            return Box::new(image().source(target).alt(run.text).class("markdown-image"));
        }
        return Box::new(sapid::anchor().href(target).label(run.text).class(class));
    }
    Box::new(
        text()
            .class(class)
            .kind(if run.marks.code {
                TextKind::Code
            } else {
                TextKind::Inline
            })
            .content(run.text),
    )
}
fn runs_widget(runs: Vec<Run>) -> Box<dyn Widget> {
    Box::new(
        view()
            .class("markdown-prose")
            .fill(runs.into_iter().map(run_widget).collect::<Vec<_>>()),
    )
}

thread_local! {static NEXT_TABLE:Cell<u64>=const{Cell::new(0)};}
fn table_widgets(header: Vec<Vec<Run>>, rows: Vec<Vec<Vec<Run>>>) -> Vec<Box<dyn Widget>> {
    let columns = header.len().max(rows.first().map_or(0, Vec::len));
    if columns == 0 {
        return Vec::new();
    }
    if columns > sapid::MAX_TABLE_COLUMNS {
        return std::iter::once(header)
            .chain(rows)
            .map(|row| {
                runs_widget(
                    row.into_iter()
                        .enumerate()
                        .flat_map(|(index, mut cell)| {
                            if index > 0 {
                                cell.insert(
                                    0,
                                    Run {
                                        text: " | ".into(),
                                        marks: Marks::default(),
                                        target: None,
                                        image: false,
                                    },
                                );
                            }
                            cell
                        })
                        .collect(),
                )
            })
            .collect();
    }
    let mut output = Vec::<Box<dyn Widget>>::new();
    let rows_per_table = sapid::MAX_TABLE_ROWS
        .saturating_sub(1)
        .min((sapid::MAX_TABLE_CELLS / columns).saturating_sub(1))
        .max(1);
    let count = rows.len().max(1);
    for start in (0..count).step_by(rows_per_table) {
        let key = NEXT_TABLE.with(|next| {
            let value = next.get();
            next.set(value.wrapping_add(1));
            format!("markdown-{value}")
        });
        let make_row = |index: usize, cells: Vec<Vec<Run>>, heading: bool| {
            let mut cells = cells;
            cells.resize_with(columns, Vec::new);
            sapid::table_row(format!("row-{index}")).cells(cells.into_iter().enumerate().map(
                |(column, runs)| {
                    let cell = if heading {
                        sapid::table_header_cell(format!("cell-{column}"))
                    } else {
                        sapid::table_cell(format!("cell-{column}"))
                    };
                    cell.content(move || runs_widget(runs.clone()))
                },
            ))
        };
        let mut table = sapid::table(key).class("markdown-table").bordered(true);
        if !header.is_empty() {
            table = table.head(sapid::table_head().row(make_row(0, header.clone(), true)));
        }
        let end = (start + rows_per_table).min(rows.len());
        if start < end {
            table = table.body(
                sapid::table_body().rows(
                    rows[start..end]
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(index, row)| make_row(index + 1, row, false)),
                ),
            );
        }
        output.push(Box::new(view().class("markdown-table-scroll").fill(table)));
    }
    output
}

pub fn render(content: &str) -> Box<dyn Widget> {
    let mut children = Vec::<Box<dyn Widget>>::new();
    for block in parse(content) {
        match block {
            Block::Paragraph {
                runs,
                heading: Some(level),
                ..
            } => children.push(Box::new(
                text()
                    .kind(TextKind::Heading(level))
                    .content(runs.into_iter().map(|run| run.text).collect::<String>()),
            )),
            Block::Paragraph {
                runs,
                heading: None,
                quote,
            } => children.push(Box::new(
                view()
                    .class(if quote {
                        "markdown-paragraph markdown-quote"
                    } else {
                        "markdown-paragraph"
                    })
                    .fill(runs_widget(runs)),
            )),
            Block::Code { source, language } => {
                let copy = source.clone();
                children.push(Box::new(
                    col().class("markdown-code-block").fill((
                        row().class("markdown-code-header").fill((
                            text().content(language),
                            sapid::button()
                                .class("copy-code")
                                .label("Copy code")
                                .on_press(move || {
                                    #[cfg(target_arch = "wasm32")]
                                    if let Some(window) = web_sys::window() {
                                        let _ = window.navigator().clipboard().write_text(&copy);
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    let _ = &copy;
                                }),
                        )),
                        text()
                            .class("markdown-code")
                            .kind(TextKind::Code)
                            .content(source),
                    )),
                ));
            }
            Block::Table { header, rows } => children.extend(table_widgets(header, rows)),
            Block::Rule => children.push(Box::new(sapid::separator())),
        }
    }
    Box::new(col().class("markdown-content").fill(children))
}

#[cfg(test)]
mod tests {
    #[test]
    fn inline_marks_links_and_code_keep_their_semantic_styles() {
        let html=glory::ssr::render_with(|_|super::render("**bold** *italic* ~~removed~~ `code` [web](https://example.com) [bad](javascript:alert(1))")).unwrap();
        assert!(html.contains("markdown-strong"));
        assert!(html.contains("markdown-emphasis"));
        assert!(html.contains("markdown-strike"));
        assert!(html.contains("markdown-inline-code"));
        assert!(html.contains("href=\"https://example.com\""));
        assert!(!html.contains("href=\"javascript:"));
    }
    #[test]
    fn table_header_and_data_cells_remain_distinct() {
        let html = glory::ssr::render_with(|_| {
            super::render("| Name | Count |\n|---|---|\n| **Rust** | 2 |\n| Glory | 3 |")
        })
        .unwrap();
        assert!(html.contains("<table"));
        assert!(html.contains("<th"));
        assert!(html.contains("<td"));
        assert!(html.contains("Rust"));
        assert!(html.contains("markdown-strong"));
    }
    #[test]
    fn raw_html_is_escaped_and_code_fences_preserve_newlines() {
        let html = glory::ssr::render_with(|_| {
            super::render(
                "<script>alert(1)</script>\n\n```rust\nfn main() {}\n```\n\n- first\n- second",
            )
        })
        .unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("fn main() {}"));
        assert!(html.contains(">first<"));
        assert!(html.contains(">second<"));
    }
}
