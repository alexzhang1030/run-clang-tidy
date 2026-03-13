use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Diagnostic {
    file: String,
    line: usize,
    column: usize,
    level: String,
    message: String,
    rule_id: Option<String>,
    source_line: Option<String>,
    caret_line: Option<String>,
    notes: Vec<DiagnosticNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticNote {
    file: String,
    line: usize,
    column: usize,
    message: String,
    source_line: Option<String>,
    caret_line: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    pub rendered: Option<String>,
    pub report: Report,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    results: Vec<SarifResult>,
}

impl Report {
    pub fn append(&mut self, other: Report) {
        self.results.extend(other.results);
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    pub fn write_to(&self, mut writer: impl io::Write) -> io::Result<()> {
        let payload = SarifRoot {
            version: "2.1.0",
            runs: vec![SarifRun {
                tool: SarifTool {
                    driver: SarifDriver { name: "clang-tidy" },
                },
                results: self.results.clone(),
            }],
        };

        serde_json::to_writer_pretty(&mut writer, &payload).map_err(io::Error::other)?;
        writer.write_all(b"\n")
    }
}

pub fn analyze(raw: &str, strip_root: Option<&Path>) -> Analysis {
    let diagnostics = parse_diagnostics(raw, strip_root);
    if diagnostics.is_empty() {
        return Analysis::default();
    }

    let rendered = render_diagnostics(&diagnostics);
    let report = Report {
        results: diagnostics.iter().map(SarifResult::from).collect(),
    };

    Analysis {
        rendered: Some(rendered),
        report,
    }
}

fn parse_diagnostics(raw: &str, strip_root: Option<&Path>) -> Vec<Diagnostic> {
    let lines: Vec<_> = raw.lines().collect();
    let mut idx = 0usize;
    let mut diagnostics = Vec::new();

    while idx < lines.len() {
        let Some(parsed) = parse_header(lines[idx], strip_root) else {
            idx += 1;
            continue;
        };

        let mut diagnostic = Diagnostic {
            file: parsed.file,
            line: parsed.line,
            column: parsed.column,
            level: parsed.level,
            message: parsed.message,
            rule_id: parsed.rule_id,
            source_line: None,
            caret_line: None,
            notes: Vec::new(),
        };

        idx += 1;
        attach_context(
            &lines,
            &mut idx,
            &mut diagnostic.source_line,
            &mut diagnostic.caret_line,
        );

        while idx < lines.len() {
            let Some(note) = parse_header(lines[idx], strip_root) else {
                break;
            };

            if note.level != "note" {
                break;
            }

            let mut note_entry = DiagnosticNote {
                file: note.file,
                line: note.line,
                column: note.column,
                message: note.message,
                source_line: None,
                caret_line: None,
            };

            idx += 1;
            attach_context(
                &lines,
                &mut idx,
                &mut note_entry.source_line,
                &mut note_entry.caret_line,
            );
            diagnostic.notes.push(note_entry);
        }

        diagnostics.push(diagnostic);
    }

    diagnostics
}

fn attach_context(
    lines: &[&str],
    idx: &mut usize,
    source_line: &mut Option<String>,
    caret_line: &mut Option<String>,
) {
    if *idx >= lines.len() {
        return;
    }

    if is_context_line(lines[*idx]) {
        *source_line = Some(lines[*idx].to_string());
        *idx += 1;
    }

    if *idx >= lines.len() {
        return;
    }

    if is_caret_line(lines[*idx]) {
        *caret_line = Some(lines[*idx].to_string());
        *idx += 1;
    }
}

fn render_diagnostics(items: &[Diagnostic]) -> String {
    let mut out = Vec::new();
    for item in items {
        out.push(render_primary(item));
        for note in &item.notes {
            out.push(render_note(note));
        }
    }
    out.join("\n\n")
}

fn render_primary(item: &Diagnostic) -> String {
    let mut lines = Vec::new();
    let level = style_level(&item.level);
    let message = match &item.rule_id {
        Some(rule_id) if !rule_id.is_empty() => format!("{} [{}]", item.message, rule_id),
        _ => item.message.clone(),
    };

    lines.push(format!("{level}: {message}"));
    lines.push(format!("  --> {}:{}:{}", item.file, item.line, item.column));

    if let Some(source_line) = &item.source_line {
        lines.push(format!("{:>4} | {}", item.line, source_line.trim_start()));
    }
    if let Some(caret_line) = &item.caret_line {
        lines.push(format!("     | {}", caret_line.trim_start()));
    }

    lines.join("\n")
}

fn render_note(item: &DiagnosticNote) -> String {
    let mut lines = vec![
        format!("note: {}", item.message),
        format!("  --> {}:{}:{}", item.file, item.line, item.column),
    ];

    if let Some(source_line) = &item.source_line {
        lines.push(format!("{:>4} | {}", item.line, source_line.trim_start()));
    }
    if let Some(caret_line) = &item.caret_line {
        lines.push(format!("     | {}", caret_line.trim_start()));
    }

    lines.join("\n")
}

fn style_level(level: &str) -> String {
    let styled = match level {
        "error" => console::style(level).red().bold(),
        "warning" => console::style(level).yellow().bold(),
        "note" => console::style(level).blue().bold(),
        _ => console::style(level).white().bold(),
    };
    styled.to_string()
}

fn is_context_line(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn is_caret_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '^' | '~' | ' ' | '-'))
}

fn parse_header(line: &str, strip_root: Option<&Path>) -> Option<HeaderMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let regex = RE.get_or_init(|| {
        Regex::new(
            r"^(?P<file>([a-zA-Z]:|)[\w/\.\- \\]+):(?P<line>\d+):(?P<column>\d+):\s+(?P<level>error|warning|info|note):\s+(?P<message>.+?)(?:\s+\[(?P<rule>[^\]]+)\])?$",
        )
        .unwrap()
    });

    let captures = regex.captures(line)?;
    let file = captures.name("file")?.as_str();
    let file = display_path(file, strip_root);

    Some(HeaderMatch {
        file,
        line: captures.name("line")?.as_str().parse().ok()?,
        column: captures.name("column")?.as_str().parse().ok()?,
        level: captures.name("level")?.as_str().to_string(),
        message: captures.name("message")?.as_str().to_string(),
        rule_id: captures.name("rule").map(|m| m.as_str().to_string()),
    })
}

fn display_path(file: &str, strip_root: Option<&Path>) -> String {
    let path = PathBuf::from(file);
    match strip_root {
        Some(strip_root) => {
            if let Ok(stripped) = path.strip_prefix(strip_root) {
                stripped.to_string_lossy().into_owned()
            } else {
                path.to_string_lossy().into_owned()
            }
        }
        None => file.to_string(),
    }
}

struct HeaderMatch {
    file: String,
    line: usize,
    column: usize,
    level: String,
    message: String,
    rule_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifRoot<'a> {
    version: &'a str,
    runs: Vec<SarifRun>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifRun {
    tool: SarifTool,
    results: Vec<SarifResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifDriver {
    name: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifResult {
    #[serde(rename = "ruleId", skip_serializing_if = "Option::is_none")]
    rule_id: Option<String>,
    level: String,
    message: SarifMessage,
    locations: Vec<SarifLocation>,
    #[serde(rename = "relatedLocations", skip_serializing_if = "Vec::is_empty")]
    related_locations: Vec<SarifRelatedLocation>,
}

impl From<&Diagnostic> for SarifResult {
    fn from(value: &Diagnostic) -> Self {
        Self {
            rule_id: value.rule_id.clone(),
            level: value.level.clone(),
            message: SarifMessage {
                text: value.message.clone(),
            },
            locations: vec![SarifLocation::new(&value.file, value.line, value.column)],
            related_locations: value
                .notes
                .iter()
                .map(|note| SarifRelatedLocation {
                    id: 0,
                    message: SarifMessage {
                        text: note.message.clone(),
                    },
                    physical_location: SarifPhysicalLocation::new(
                        &note.file,
                        note.line,
                        note.column,
                    ),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifMessage {
    text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

impl SarifLocation {
    fn new(file: &str, line: usize, column: usize) -> Self {
        Self {
            physical_location: SarifPhysicalLocation::new(file, line, column),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifRelatedLocation {
    id: u64,
    message: SarifMessage,
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

impl SarifPhysicalLocation {
    fn new(file: &str, line: usize, column: usize) -> Self {
        Self {
            artifact_location: SarifArtifactLocation {
                uri: file.to_string(),
            },
            region: SarifRegion {
                start_line: line,
                start_column: column,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: usize,
    #[serde(rename = "startColumn")]
    start_column: usize,
}

#[cfg(test)]
mod tests {
    use super::{analyze, Report};

    #[test]
    fn parses_and_renders_clang_tidy_output() {
        let raw = r#"
1 warning generated.
src/demo.cpp:8:10: warning: broken thing [demo-check]
  return str[0];
         ^~~~~~
src/demo.cpp:12:25: note: Passing null pointer value via 1st parameter 'str'
  return get_first_char(nullptr);
                        ^~~~~~~
"#;

        let analysis = analyze(raw, None);

        let rendered = analysis.rendered.expect("rendered output");
        assert!(rendered.contains("warning: broken thing [demo-check]"));
        assert!(rendered.contains("src/demo.cpp:8:10"));
        assert!(rendered.contains("note: Passing null pointer value"));

        let mut buf = Vec::new();
        analysis.report.write_to(&mut buf).unwrap();
        let json = String::from_utf8(buf).unwrap();
        assert!(json.contains("\"ruleId\": \"demo-check\""));
        assert!(json.contains("\"relatedLocations\""));
    }

    #[test]
    fn empty_report_stays_empty() {
        let analysis = analyze("no diagnostics here", None);
        assert_eq!(analysis.rendered, None);
        assert!(Report::default().is_empty());
        assert!(analysis.report.is_empty());
    }
}
