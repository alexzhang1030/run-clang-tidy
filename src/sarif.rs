use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Diagnostic {
    file: String,
    source_path: PathBuf,
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
    source_path: PathBuf,
    line: usize,
    column: usize,
    message: String,
    source_line: Option<String>,
    caret_line: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    diagnostics: Vec<Diagnostic>,
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

    #[cfg(test)]
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

    let report = Report {
        results: diagnostics.iter().map(SarifResult::from).collect(),
    };

    Analysis {
        diagnostics,
        report,
    }
}

impl Analysis {
    pub fn render(&self, filter: Option<&LevelFilter>) -> Option<String> {
        render_diagnostics(&self.diagnostics, filter)
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
            source_path: parsed.source_path,
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
                source_path: note.source_path,
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
    let source_path = resolve_path(file, strip_root);

    Some(HeaderMatch {
        file: display_path(&source_path),
        source_path,
        line: captures.name("line")?.as_str().parse().ok()?,
        column: captures.name("column")?.as_str().parse().ok()?,
        level: captures.name("level")?.as_str().to_string(),
        message: captures.name("message")?.as_str().to_string(),
        rule_id: captures.name("rule").map(|m| m.as_str().to_string()),
    })
}

fn resolve_path(file: &str, strip_root: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        return path;
    }

    match strip_root {
        Some(strip_root) => strip_root.join(path),
        None => path,
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct HeaderMatch {
    file: String,
    source_path: PathBuf,
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
            locations: vec![SarifLocation::new(
                &value.file,
                value.source_path.clone(),
                value.line,
                value.column,
            )],
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
                        note.source_path.clone(),
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
    fn new(file: &str, resolved_path: PathBuf, line: usize, column: usize) -> Self {
        Self {
            physical_location: SarifPhysicalLocation::new(file, resolved_path, line, column),
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
    #[serde(skip_serializing)]
    resolved_path: PathBuf,
}

impl SarifPhysicalLocation {
    fn new(file: &str, resolved_path: PathBuf, line: usize, column: usize) -> Self {
        Self {
            artifact_location: SarifArtifactLocation {
                uri: file.to_string(),
            },
            region: SarifRegion {
                start_line: line,
                start_column: column,
            },
            resolved_path,
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

#[derive(Debug, Clone)]
pub struct LevelFilter {
    levels: BTreeSet<String>,
}

impl LevelFilter {
    pub fn parse(value: &str) -> eyre::Result<Self> {
        let levels: BTreeSet<_> = value
            .split(',')
            .map(str::trim)
            .filter(|level| !level.is_empty())
            .map(str::to_owned)
            .collect();

        if levels.is_empty() {
            return Err(eyre::eyre!(
                "Expected at least one diagnostic level in --filter"
            ));
        }

        for level in &levels {
            if !matches!(level.as_str(), "error" | "warning" | "note" | "info") {
                return Err(eyre::eyre!(format!(
                    "Unsupported diagnostic level '{level}' in --filter"
                )));
            }
        }

        Ok(Self { levels })
    }

    fn allows(&self, level: &str) -> bool {
        self.levels.contains(level)
    }
}

fn render_diagnostics(items: &[Diagnostic], filter: Option<&LevelFilter>) -> Option<String> {
    let mut out = Vec::new();
    let mut warning_count = 0usize;
    let mut error_count = 0usize;

    for item in items {
        if filter
            .map(|filter| filter.allows(&item.level))
            .unwrap_or(true)
        {
            if item.level == "warning" {
                warning_count += 1;
            } else if item.level == "error" {
                error_count += 1;
            }
            out.push(render_block(
                &item.level,
                &item.message,
                item.source_path.as_path(),
                item.line,
                item.column,
                item.source_line.as_deref(),
                item.caret_line.as_deref(),
            ));
        }

        for note in &item.notes {
            if filter.map(|filter| filter.allows("note")).unwrap_or(true) {
                out.push(render_block(
                    "note",
                    &note.message,
                    note.source_path.as_path(),
                    note.line,
                    note.column,
                    note.source_line.as_deref(),
                    note.caret_line.as_deref(),
                ));
            }
        }
    }

    if out.is_empty() {
        return None;
    }

    if warning_count > 0 {
        out.push(format!(
            "{}: {} warnings emitted",
            style_level("warning"),
            warning_count
        ));
    }
    if error_count > 0 {
        out.push(format!(
            "{}: {} errors emitted",
            style_level("error"),
            error_count
        ));
    }

    Some(out.join("\n\n"))
}

fn render_block(
    level: &str,
    message: &str,
    path: &Path,
    line: usize,
    column: usize,
    source_line: Option<&str>,
    caret_line: Option<&str>,
) -> String {
    let gutter_width = line.to_string().len();
    let pipe = console::style("│").dim();
    let location_prefix = console::style("┌─").dim();

    let mut lines = vec![
        format!("{}: {}", style_level(level), message),
        format!(
            "    {} {}:{}:{}",
            location_prefix,
            console::style(path.to_string_lossy()).cyan().bold(),
            line,
            column
        ),
        format!("    {}", pipe),
    ];

    if let Some(source_line) = source_line {
        lines.push(format!(
            "{:>width$} {} {}",
            line,
            pipe,
            source_line.trim_end(),
            width = gutter_width
        ));
    }

    if let Some(caret_line) = caret_line {
        lines.push(format!(
            "{:>width$} {} {}",
            "",
            pipe,
            style_caret(level, caret_line.trim_end()),
            width = gutter_width
        ));
    }

    lines.join("\n")
}

fn style_caret(level: &str, caret_line: &str) -> String {
    let style = match level {
        "error" => console::Style::new().red().bold(),
        "warning" => console::Style::new().yellow().bold(),
        "note" => console::Style::new().blue().bold(),
        _ => console::Style::new().white().bold(),
    };

    style.apply_to(caret_line).to_string()
}

#[cfg(test)]
mod tests {
    use super::{analyze, LevelFilter, Report};

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

        let rendered = analysis.render(None).expect("rendered output");
        assert!(rendered.contains("warning: broken thing"));
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
        assert_eq!(analysis.render(None), None);
        assert!(Report::default().is_empty());
        assert!(analysis.report.is_empty());
    }

    #[test]
    fn filter_keeps_requested_levels_only() {
        let raw = r#"
src/demo.cpp:8:10: warning: broken thing [demo-check]
  return str[0];
         ^~~~~~
src/demo.cpp:12:25: note: Passing null pointer value via 1st parameter 'str'
  return get_first_char(nullptr);
                        ^~~~~~~
"#;

        let analysis = analyze(raw, None);
        let filter = LevelFilter::parse("note").unwrap();
        let rendered = analysis.render(Some(&filter)).expect("filtered output");

        assert!(!rendered.contains("warning: broken thing"));
        assert!(rendered.contains("note: Passing null pointer value"));
    }
}
