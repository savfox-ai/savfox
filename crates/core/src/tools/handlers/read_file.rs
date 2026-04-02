use std::collections::VecDeque;
use std::path::PathBuf;

use savfox_utils::string::take_bytes_at_char_boundary;
use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const MAX_LINE_LENGTH: usize = 500;
const TAB_WIDTH: usize = 4;

// TODO(jif) add support for block comments
const COMMENT_PREFIXES: &[&str] = &["#", "//", "--"];

/// JSON arguments accepted by the `read_file` tool handler.
#[derive(Deserialize)]
struct ReadFileArgs {
    /// Absolute path to the file that will be read.
    file_path: String,
    /// 1-indexed line number to start reading from; defaults to 1.
    #[serde(default = "defaults::offset")]
    offset: usize,
    /// Maximum number of lines to return; defaults to 2000.
    #[serde(default = "defaults::limit")]
    limit: usize,
    /// Determines whether the handler reads a simple slice or indentation-aware block.
    #[serde(default)]
    mode: ReadMode,
    /// Optional indentation configuration used when `mode` is `Indentation`.
    #[serde(default)]
    indentation: Option<IndentationArgs>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ReadMode {
    #[default]
    Slice,
    Indentation,
}
/// Additional configuration for indentation-aware reads.
#[derive(Deserialize, Clone)]
struct IndentationArgs {
    /// Optional explicit anchor line; defaults to `offset` when omitted.
    #[serde(default)]
    anchor_line: Option<usize>,
    /// Maximum indentation depth to collect; `0` means unlimited.
    #[serde(default = "defaults::max_levels")]
    max_levels: usize,
    /// Whether to include sibling blocks at the same indentation level.
    #[serde(default = "defaults::include_siblings")]
    include_siblings: bool,
    /// Whether to include header lines above the anchor block. This made on a best effort basis.
    #[serde(default = "defaults::include_header")]
    include_header: bool,
    /// Optional hard cap on returned lines; defaults to the global `limit`.
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Clone, Debug)]
struct LineRecord {
    number: usize,
    raw: String,
    display: String,
    indent: usize,
}

impl LineRecord {
    fn trimmed(&self) -> &str {
        self.raw.trim_start()
    }

    fn is_blank(&self) -> bool {
        self.trimmed().is_empty()
    }

    fn is_comment(&self) -> bool {
        COMMENT_PREFIXES
            .iter()
            .any(|prefix| self.raw.trim().starts_with(prefix))
    }
}

pub struct ReadFileHandler;

#[async_trait::async_trait]
impl ToolHandler for ReadFileHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ReadFileHandler received unsupported payload"),
        };
        let args: ReadFileArgs = parse_arguments(&arguments)?;

        let ReadFileArgs {
            file_path,
            offset,
            limit,
            mode,
            indentation,
        } = args;

        if offset == 0 {
            return model_err("offset must be a 1-indexed line number");
        }

        if limit == 0 {
            return model_err("limit must be greater than zero");
        }

        let path = PathBuf::from(&file_path);
        if !path.is_absolute() {
            return model_err("file_path must be an absolute path");
        }

        let collected = match mode {
            ReadMode::Slice => slice::read(&path, offset, limit).await?,
            ReadMode::Indentation => {
                let indentation = indentation.unwrap_or_default();
                indentation::read_block(&path, offset, limit, indentation).await?
            }
        };
        let content = collected.join("\n");
        let wrapped = crate::external_content::wrap_external_content(&file_path, &content);
        Ok(ToolOutput::ok(wrapped))
    }
}

mod slice {
    use std::path::Path;

    use tokio::fs::File;
    use tokio::io::{AsyncBufReadExt, BufReader};

    use crate::function_tool::{FunctionCallError, model_err};
    use crate::tools::handlers::read_file::format_line;

    pub async fn read(
        path: &Path,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<String>, FunctionCallError> {
        let file = File::open(path)
            .await
            .or_else(|err| model_err(format!("failed to read file: {err}")))?;

        let mut reader = BufReader::new(file);
        let mut collected = Vec::new();
        let mut seen = 0usize;
        let mut buffer = Vec::new();

        loop {
            buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut buffer)
                .await
                .or_else(|err| model_err(format!("failed to read file: {err}")))?;

            if bytes_read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\n') {
                buffer.pop();
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
            }

            seen += 1;

            if seen < offset {
                continue;
            }

            if collected.len() == limit {
                break;
            }

            let formatted = format_line(&buffer);
            collected.push(format!("L{seen}: {formatted}"));

            if collected.len() == limit {
                break;
            }
        }

        if seen < offset {
            return model_err("offset exceeds file length");
        }

        Ok(collected)
    }
}

mod indentation {
    use std::collections::VecDeque;
    use std::path::Path;

    use tokio::fs::File;
    use tokio::io::{AsyncBufReadExt, BufReader};

    use crate::function_tool::{FunctionCallError, model_err};
    use crate::tools::handlers::read_file::{
        IndentationArgs, LineRecord, TAB_WIDTH, format_line, trim_empty_lines,
    };

    pub async fn read_block(
        path: &Path,
        offset: usize,
        limit: usize,
        options: IndentationArgs,
    ) -> Result<Vec<String>, FunctionCallError> {
        let anchor_line = options.anchor_line.unwrap_or(offset);
        if anchor_line == 0 {
            return model_err("anchor_line must be a 1-indexed line number");
        }

        let guard_limit = options.max_lines.unwrap_or(limit);
        if guard_limit == 0 {
            return model_err("max_lines must be greater than zero");
        }

        let collected = collect_file_lines(path).await?;
        if collected.is_empty() || anchor_line > collected.len() {
            return model_err("anchor_line exceeds file length");
        }

        let anchor_index = anchor_line - 1;
        let effective_indents = compute_effective_indents(&collected);
        let anchor_indent = effective_indents[anchor_index];

        // Compute the min indent
        let min_indent = if options.max_levels == 0 {
            0
        } else {
            anchor_indent.saturating_sub(options.max_levels * TAB_WIDTH)
        };

        // Cap requested lines by guard_limit and file length
        let final_limit = limit.min(guard_limit).min(collected.len());

        if final_limit == 1 {
            return Ok(vec![format!(
                "L{}: {}",
                collected[anchor_index].number, collected[anchor_index].display
            )]);
        }

        // Cursors
        let mut i: isize = anchor_index as isize - 1; // up (inclusive)
        let mut j: usize = anchor_index + 1; // down (inclusive)
        let mut i_counter_min_indent = 0;
        let mut j_counter_min_indent = 0;

        let mut out = VecDeque::with_capacity(limit);
        out.push_back(&collected[anchor_index]);

        while out.len() < final_limit {
            let mut progressed = 0;

            // Up.
            if i >= 0 {
                let iu = i as usize;
                if effective_indents[iu] >= min_indent {
                    out.push_front(&collected[iu]);
                    progressed += 1;
                    i -= 1;

                    // We do not include the siblings (not applied to comments).
                    if effective_indents[iu] == min_indent && !options.include_siblings {
                        let allow_header_comment =
                            options.include_header && collected[iu].is_comment();
                        let can_take_line = allow_header_comment || i_counter_min_indent == 0;

                        if can_take_line {
                            i_counter_min_indent += 1;
                        } else {
                            // This line shouldn't have been taken.
                            out.pop_front();
                            progressed -= 1;
                            i = -1; // consider using Option<usize> or a control flag instead of a sentinel
                        }
                    }

                    // Short-cut.
                    if out.len() >= final_limit {
                        break;
                    }
                } else {
                    // Stop moving up.
                    i = -1;
                }
            }

            // Down.
            if j < collected.len() {
                let ju = j;
                if effective_indents[ju] >= min_indent {
                    out.push_back(&collected[ju]);
                    progressed += 1;
                    j += 1;

                    // We do not include the siblings (applied to comments).
                    if effective_indents[ju] == min_indent && !options.include_siblings {
                        if j_counter_min_indent > 0 {
                            // This line shouldn't have been taken.
                            out.pop_back();
                            progressed -= 1;
                            j = collected.len();
                        }
                        j_counter_min_indent += 1;
                    }
                } else {
                    // Stop moving down.
                    j = collected.len();
                }
            }

            if progressed == 0 {
                break;
            }
        }

        // Trim empty lines
        trim_empty_lines(&mut out);

        Ok(out
            .into_iter()
            .map(|record| format!("L{}: {}", record.number, record.display))
            .collect())
    }

    async fn collect_file_lines(path: &Path) -> Result<Vec<LineRecord>, FunctionCallError> {
        let file = File::open(path)
            .await
            .or_else(|err| model_err(format!("failed to read file: {err}")))?;

        let mut reader = BufReader::new(file);
        let mut buffer = Vec::new();
        let mut lines = Vec::new();
        let mut number = 0usize;

        loop {
            buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut buffer)
                .await
                .or_else(|err| model_err(format!("failed to read file: {err}")))?;

            if bytes_read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\n') {
                buffer.pop();
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
            }

            number += 1;
            let raw = String::from_utf8_lossy(&buffer).into_owned();
            let indent = measure_indent(&raw);
            let display = format_line(&buffer);
            lines.push(LineRecord {
                number,
                raw,
                display,
                indent,
            });
        }

        Ok(lines)
    }

    fn compute_effective_indents(records: &[LineRecord]) -> Vec<usize> {
        let mut effective = Vec::with_capacity(records.len());
        let mut previous_indent = 0usize;
        for record in records {
            if record.is_blank() {
                effective.push(previous_indent);
            } else {
                previous_indent = record.indent;
                effective.push(previous_indent);
            }
        }
        effective
    }

    fn measure_indent(line: &str) -> usize {
        line.chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
            .sum()
    }
}

fn format_line(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    if decoded.len() > MAX_LINE_LENGTH {
        take_bytes_at_char_boundary(&decoded, MAX_LINE_LENGTH).to_owned()
    } else {
        decoded.into_owned()
    }
}

fn trim_empty_lines(out: &mut VecDeque<&LineRecord>) {
    while matches!(out.front(), Some(line) if line.raw.trim().is_empty()) {
        out.pop_front();
    }
    while matches!(out.back(), Some(line) if line.raw.trim().is_empty()) {
        out.pop_back();
    }
}

mod defaults {
    use super::*;

    impl Default for IndentationArgs {
        fn default() -> Self {
            Self {
                anchor_line: None,
                max_levels: max_levels(),
                include_siblings: include_siblings(),
                include_header: include_header(),
                max_lines: None,
            }
        }
    }

    pub fn offset() -> usize {
        1
    }

    pub fn limit() -> usize {
        2000
    }

    pub fn max_levels() -> usize {
        0
    }

    pub fn include_siblings() -> bool {
        false
    }

    pub fn include_header() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::NamedTempFile;

    use super::indentation::read_block;
    use super::slice::read;
    use super::*;

    #[tokio::test]
    async fn reads_requested_range() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "alpha
beta
gamma
"
        )?;

        let lines = read(temp.path(), 2, 2).await?;
        assert_eq!(lines, vec!["L2: beta".to_owned(), "L3: gamma".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn errors_when_offset_exceeds_length() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        writeln!(temp, "only")?;

        let err = read(temp.path(), 3, 1)
            .await
            .expect_err("offset exceeds length");
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("offset exceeds file length".to_owned())
        );
        Ok(())
    }

    #[tokio::test]
    async fn reads_non_utf8_lines() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        temp.as_file_mut().write_all(b"\xff\xfe\nplain\n")?;

        let lines = read(temp.path(), 1, 2).await?;
        let expected_first = format!("L1: {}{}", '\u{FFFD}', '\u{FFFD}');
        assert_eq!(lines, vec![expected_first, "L2: plain".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn trims_crlf_endings() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(temp, "one\r\ntwo\r\n")?;

        let lines = read(temp.path(), 1, 2).await?;
        assert_eq!(lines, vec!["L1: one".to_owned(), "L2: two".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn respects_limit_even_with_more_lines() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "first
second
third
"
        )?;

        let lines = read(temp.path(), 1, 2).await?;
        assert_eq!(
            lines,
            vec!["L1: first".to_owned(), "L2: second".to_owned()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn truncates_lines_longer_than_max_length() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        let long_line = "x".repeat(MAX_LINE_LENGTH + 50);
        writeln!(temp, "{long_line}")?;

        let lines = read(temp.path(), 1, 1).await?;
        let expected = "x".repeat(MAX_LINE_LENGTH);
        assert_eq!(lines, vec![format!("L1: {expected}")]);
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_captures_block() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "fn outer() {{
    if cond {{
        inner();
    }}
    tail();
}}
"
        )?;

        let options = IndentationArgs {
            anchor_line: Some(3),
            include_siblings: false,
            max_levels: 1,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 3, 10, options).await?;

        assert_eq!(
            lines,
            vec![
                "L2:     if cond {".to_owned(),
                "L3:         inner();".to_owned(),
                "L4:     }".to_owned()
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_expands_parents() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "mod root {{
    fn outer() {{
        if cond {{
            inner();
        }}
    }}
}}
"
        )?;

        let mut options = IndentationArgs {
            anchor_line: Some(4),
            max_levels: 2,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 4, 50, options.clone()).await?;
        assert_eq!(
            lines,
            vec![
                "L2:     fn outer() {".to_owned(),
                "L3:         if cond {".to_owned(),
                "L4:             inner();".to_owned(),
                "L5:         }".to_owned(),
                "L6:     }".to_owned(),
            ]
        );

        options.max_levels = 3;
        let expanded = read_block(temp.path(), 4, 50, options).await?;
        assert_eq!(
            expanded,
            vec![
                "L1: mod root {".to_owned(),
                "L2:     fn outer() {".to_owned(),
                "L3:         if cond {".to_owned(),
                "L4:             inner();".to_owned(),
                "L5:         }".to_owned(),
                "L6:     }".to_owned(),
                "L7: }".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_respects_sibling_flag() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "fn wrapper() {{
    if first {{
        do_first();
    }}
    if second {{
        do_second();
    }}
}}
"
        )?;

        let mut options = IndentationArgs {
            anchor_line: Some(3),
            include_siblings: false,
            max_levels: 1,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 3, 50, options.clone()).await?;
        assert_eq!(
            lines,
            vec![
                "L2:     if first {".to_owned(),
                "L3:         do_first();".to_owned(),
                "L4:     }".to_owned(),
            ]
        );

        options.include_siblings = true;
        let with_siblings = read_block(temp.path(), 3, 50, options).await?;
        assert_eq!(
            with_siblings,
            vec![
                "L2:     if first {".to_owned(),
                "L3:         do_first();".to_owned(),
                "L4:     }".to_owned(),
                "L5:     if second {".to_owned(),
                "L6:         do_second();".to_owned(),
                "L7:     }".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_handles_python_sample() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "class Foo:
    def __init__(self, size):
        self.size = size
    def double(self, value):
        if value is None:
            return 0
        result = value * self.size
        return result
class Bar:
    def compute(self):
        helper = Foo(2)
        return helper.double(5)
"
        )?;

        let options = IndentationArgs {
            anchor_line: Some(7),
            include_siblings: true,
            max_levels: 1,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 1, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L2:     def __init__(self, size):".to_owned(),
                "L3:         self.size = size".to_owned(),
                "L4:     def double(self, value):".to_owned(),
                "L5:         if value is None:".to_owned(),
                "L6:             return 0".to_owned(),
                "L7:         result = value * self.size".to_owned(),
                "L8:         return result".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore]
    async fn indentation_mode_handles_javascript_sample() -> anyhow::Result<()> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "export function makeThing() {{
    const cache = new Map();
    function ensure(key) {{
        if (!cache.has(key)) {{
            cache.set(key, []);
        }}
        return cache.get(key);
    }}
    const handlers = {{
        init() {{
            console.log(\"init\");
        }},
        run() {{
            if (Math.random() > 0.5) {{
                return \"heads\";
            }}
            return \"tails\";
        }},
    }};
    return {{ cache, handlers }};
}}
export function other() {{
    return makeThing();
}}
"
        )?;

        let options = IndentationArgs {
            anchor_line: Some(15),
            max_levels: 1,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 15, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L10:         init() {".to_owned(),
                "L11:             console.log(\"init\");".to_owned(),
                "L12:         },".to_owned(),
                "L13:         run() {".to_owned(),
                "L14:             if (Math.random() > 0.5) {".to_owned(),
                "L15:                 return \"heads\";".to_owned(),
                "L16:             }".to_owned(),
                "L17:             return \"tails\";".to_owned(),
                "L18:         },".to_owned(),
            ]
        );
        Ok(())
    }

    fn write_cpp_sample() -> anyhow::Result<NamedTempFile> {
        let mut temp = NamedTempFile::new()?;
        use std::io::Write as _;
        write!(
            temp,
            "#include <vector>
#include <string>

namespace sample {{
class Runner {{
public:
    void setup() {{
        if (enabled_) {{
            init();
        }}
    }}

    // Run the code
    int run() const {{
        switch (mode_) {{
            case Mode::Fast:
                return fast();
            case Mode::Slow:
                return slow();
            default:
                return fallback();
        }}
    }}

private:
    bool enabled_ = false;
    Mode mode_ = Mode::Fast;

    int fast() const {{
        return 1;
    }}
}};
}}  // namespace sample
"
        )?;
        Ok(temp)
    }

    #[tokio::test]
    async fn indentation_mode_handles_cpp_sample_shallow() -> anyhow::Result<()> {
        let temp = write_cpp_sample()?;

        let options = IndentationArgs {
            include_siblings: false,
            anchor_line: Some(18),
            max_levels: 1,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 18, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L15:         switch (mode_) {".to_owned(),
                "L16:             case Mode::Fast:".to_owned(),
                "L17:                 return fast();".to_owned(),
                "L18:             case Mode::Slow:".to_owned(),
                "L19:                 return slow();".to_owned(),
                "L20:             default:".to_owned(),
                "L21:                 return fallback();".to_owned(),
                "L22:         }".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_handles_cpp_sample() -> anyhow::Result<()> {
        let temp = write_cpp_sample()?;

        let options = IndentationArgs {
            include_siblings: false,
            anchor_line: Some(18),
            max_levels: 2,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 18, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L13:     // Run the code".to_owned(),
                "L14:     int run() const {".to_owned(),
                "L15:         switch (mode_) {".to_owned(),
                "L16:             case Mode::Fast:".to_owned(),
                "L17:                 return fast();".to_owned(),
                "L18:             case Mode::Slow:".to_owned(),
                "L19:                 return slow();".to_owned(),
                "L20:             default:".to_owned(),
                "L21:                 return fallback();".to_owned(),
                "L22:         }".to_owned(),
                "L23:     }".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_handles_cpp_sample_no_headers() -> anyhow::Result<()> {
        let temp = write_cpp_sample()?;

        let options = IndentationArgs {
            include_siblings: false,
            include_header: false,
            anchor_line: Some(18),
            max_levels: 2,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 18, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L14:     int run() const {".to_owned(),
                "L15:         switch (mode_) {".to_owned(),
                "L16:             case Mode::Fast:".to_owned(),
                "L17:                 return fast();".to_owned(),
                "L18:             case Mode::Slow:".to_owned(),
                "L19:                 return slow();".to_owned(),
                "L20:             default:".to_owned(),
                "L21:                 return fallback();".to_owned(),
                "L22:         }".to_owned(),
                "L23:     }".to_owned(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn indentation_mode_handles_cpp_sample_siblings() -> anyhow::Result<()> {
        let temp = write_cpp_sample()?;

        let options = IndentationArgs {
            include_siblings: true,
            include_header: false,
            anchor_line: Some(18),
            max_levels: 2,
            ..Default::default()
        };

        let lines = read_block(temp.path(), 18, 200, options).await?;
        assert_eq!(
            lines,
            vec![
                "L7:     void setup() {".to_owned(),
                "L8:         if (enabled_) {".to_owned(),
                "L9:             init();".to_owned(),
                "L10:         }".to_owned(),
                "L11:     }".to_owned(),
                "L12: ".to_owned(),
                "L13:     // Run the code".to_owned(),
                "L14:     int run() const {".to_owned(),
                "L15:         switch (mode_) {".to_owned(),
                "L16:             case Mode::Fast:".to_owned(),
                "L17:                 return fast();".to_owned(),
                "L18:             case Mode::Slow:".to_owned(),
                "L19:                 return slow();".to_owned(),
                "L20:             default:".to_owned(),
                "L21:                 return fallback();".to_owned(),
                "L22:         }".to_owned(),
                "L23:     }".to_owned(),
            ]
        );
        Ok(())
    }
}
