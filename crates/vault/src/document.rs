use anyhow::{Context, Result, ensure};
use mcbtodo_domain::recurrence::Reminder;
use mcbtodo_domain::{
    CalendarEvent, EvidenceRef, InboxEntry, Project, Task, TimeBlock, WorkUnit, WorkUnitStatus,
    WorkflowRevisionHash, time::Deadline,
};
use serde::{Deserialize, Serialize};
use serde_saphyr::granit_parser::{Event, Parser, ScalarStyle, Span, StructureStyle};

#[derive(Deserialize)]
#[serde(tag = "schema")]
pub(crate) enum Metadata {
    #[serde(rename = "mcbtodo.project/v1")]
    Project(Project),
    #[serde(rename = "mcbtodo.task/v1")]
    Task(Task),
    #[serde(rename = "mcbtodo.work-unit/v1")]
    Unit(WorkUnit),
    #[serde(rename = "mcbtodo.calendar-event/v1")]
    Event(CalendarEvent),
    #[serde(rename = "mcbtodo.reminder/v1")]
    Reminder(Reminder),
    #[serde(rename = "mcbtodo.inbox-entry/v1")]
    Inbox(InboxEntry),
}

pub(super) fn split(source: &str) -> Result<(&str, &str, &str, &str)> {
    let mut lines = source.split_inclusive('\n');
    let opening = lines
        .next()
        .filter(|line| is_delimiter(line))
        .context("expected opening --- on line 1")?;
    let (header_end, closing_len) = lines
        .scan(opening.len(), |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .find(|(_, line)| is_delimiter(line))
        .map(|(offset, line)| (offset, line.len()))
        .context("missing closing --- for YAML front matter")?;
    Ok((
        opening,
        &source[opening.len()..header_end],
        &source[header_end..header_end + closing_len],
        &source[header_end + closing_len..],
    ))
}

pub(crate) fn parse(source: &str) -> Result<(String, Metadata)> {
    let (_, header, _, body) = split(source)?;
    let title = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
        .context(
            "the Markdown body must begin with a non-empty # title without control characters",
        )?;
    let header = serde_saphyr::from_str(header).context("invalid YAML front matter")?;
    Ok((title.to_owned(), header))
}

pub(crate) fn render(
    schema: &'static str,
    data: &impl Serialize,
    title: &str,
    body: &str,
) -> Result<String> {
    ensure!(
        !title.trim().is_empty() && !title.chars().any(char::is_control),
        "a title must be non-empty and contain no control characters"
    );
    #[derive(Serialize)]
    struct Header<'a, T> {
        schema: &'static str,
        #[serde(flatten)]
        data: &'a T,
    }
    let header =
        serde_saphyr::to_string(&Header { schema, data }).context("serializing front matter")?;
    let source = format!("---\n{header}---\n\n# {title}\n\n{body}");
    parse(&source)?;
    Ok(source)
}

pub(crate) fn complete_unit(
    source: &str,
    unit: &WorkUnit,
    evidence: &[EvidenceRef],
) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let newline = newline(opening);
    let mut header = if unit.status == WorkUnitStatus::Completed {
        header.to_owned()
    } else {
        set_scalar(header, newline, "status", "completed", false)?
    };
    if !evidence.is_empty() {
        let items = evidence.iter().map(flow_item).collect::<Result<Vec<_>>>()?;
        header = append_items(&header, newline, "evidence", &items)?;
    }
    let updated = format!("{opening}{header}{closing}{body}");
    let (_, Metadata::Unit(updated_unit)) = parse(&updated)? else {
        anyhow::bail!("expected a work unit after completing it");
    };
    ensure!(
        updated_unit.status == WorkUnitStatus::Completed,
        "status edit did not produce a completed work unit"
    );
    ensure!(
        updated_unit
            .evidence
            .iter()
            .eq(unit.evidence.iter().chain(evidence)),
        "edit did not preserve and append the requested evidence"
    );
    Ok(updated)
}

pub(crate) fn bind_workflow(source: &str, revision: &WorkflowRevisionHash) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let header = set_scalar(
        header,
        newline(opening),
        "workflow_revision",
        revision.as_str(),
        true,
    )?;
    let updated = format!("{opening}{header}{closing}{body}");
    let (_, Metadata::Unit(unit)) = parse(&updated)? else {
        anyhow::bail!("expected a work unit after binding its workflow");
    };
    ensure!(
        unit.workflow_revision.as_ref() == Some(revision),
        "edit did not bind the requested workflow revision"
    );
    Ok(updated)
}

pub(crate) fn set_inbox_project(source: &str, project_id: mcbtodo_domain::Id) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let header = set_scalar(
        header,
        newline(opening),
        "project_id",
        &project_id.to_string(),
        false,
    )?;
    let updated = format!("{opening}{header}{closing}{body}");
    let (_, Metadata::Inbox(entry)) = parse(&updated)? else {
        anyhow::bail!("expected an Inbox entry after triage");
    };
    ensure!(
        entry.project_id == Some(project_id),
        "edit did not assign the requested Project"
    );
    Ok(updated)
}

pub(crate) fn inbox_task_content(source: &str) -> Result<(String, String)> {
    let (title, metadata) = parse(source)?;
    ensure!(
        matches!(metadata, Metadata::Inbox(_)),
        "expected an Inbox entry"
    );
    let (_, _, _, markdown) = split(source)?;
    let content = markdown
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .skip(1)
        .skip_while(|line| line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((title, content))
}

pub(crate) fn set_dependencies(
    source: &str,
    dependencies: &[mcbtodo_domain::Id],
) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let mapping = mapping_field(header, "depends_on")?;
    let value = serde_saphyr::to_string(&serde_saphyr::FlowSeq(dependencies))
        .context("serializing WorkUnit dependencies")?
        .trim()
        .to_owned();
    let mut updated_header = header.to_owned();
    if let Some(field) = mapping.field {
        let start = field
            .start
            .1
            .start
            .byte_offset()
            .context("missing dependency offset")?;
        let end = field
            .end
            .end
            .byte_offset()
            .context("missing dependency end offset")?;
        updated_header.replace_range(start..end, &value);
    } else {
        insert_field(
            &mut updated_header,
            mapping.root,
            &format!("depends_on: {value}"),
            newline(opening),
        )?;
    }
    let updated = format!("{opening}{updated_header}{closing}{body}");
    let (_, Metadata::Unit(unit)) = parse(&updated)? else {
        anyhow::bail!("expected a work unit after editing dependencies");
    };
    ensure!(
        unit.depends_on == dependencies,
        "dependency edit did not produce the requested prerequisites"
    );
    Ok(updated)
}

pub(crate) fn set_deadline(source: &str, deadline: Option<Deadline>) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let mapping = mapping_field(header, "deadline")?;
    let value = match deadline {
        Some(deadline) => serde_saphyr::to_string(&serde_saphyr::FlowMap(deadline))
            .context("serializing deadline")?
            .trim()
            .to_owned(),
        None => "null".to_owned(),
    };
    let mut updated_header = header.to_owned();
    if let Some(field) = mapping.field {
        let start = field
            .start
            .1
            .start
            .byte_offset()
            .context("missing deadline offset")?;
        let end = field
            .end
            .end
            .byte_offset()
            .context("missing deadline end offset")?;
        let replacement = match field.start.0 {
            Event::MappingStart(StructureStyle::Block, ..) => {
                format!("{value}{}", newline(opening))
            }
            _ => value,
        };
        updated_header.replace_range(start..end, &replacement);
    } else {
        insert_field(
            &mut updated_header,
            mapping.root,
            &format!("deadline: {value}"),
            newline(opening),
        )?;
    }
    let updated = format!("{opening}{updated_header}{closing}{body}");
    let (_, metadata) = parse(&updated)?;
    match metadata {
        Metadata::Project(project) => ensure!(project.deadline == deadline),
        Metadata::Task(task) => ensure!(task.deadline == deadline),
        Metadata::Unit(unit) => ensure!(unit.deadline == deadline),
        _ => anyhow::bail!("deadlines can only be set on projects, tasks and work units"),
    }
    Ok(updated)
}

pub(crate) fn append_time_block(
    source: &str,
    unit: &WorkUnit,
    block: &TimeBlock,
) -> Result<String> {
    let (opening, header, closing, body) = split(source)?;
    let header = append_items(
        header,
        newline(opening),
        "time_blocks",
        &[flow_item(block)?],
    )?;
    let updated = format!("{opening}{header}{closing}{body}");
    let (_, Metadata::Unit(updated_unit)) = parse(&updated)? else {
        anyhow::bail!("expected a work unit after the time-block edit");
    };
    let expected = unit.time_blocks.iter().chain([block]);
    ensure!(
        updated_unit.time_blocks.len() == unit.time_blocks.len() + 1
            && updated_unit
                .time_blocks
                .iter()
                .zip(expected)
                .all(|(left, right)| {
                    left.when == right.when && left.availability == right.availability
                }),
        "edit did not append exactly one time block"
    );
    Ok(updated)
}

struct Field<'a> {
    start: (Event<'a>, Span),
    end: Span,
    node_depth: Option<usize>,
    sequence_end: Option<Span>,
    last_item: Option<Span>,
}

struct MappingField<'a> {
    root: (StructureStyle, Span),
    field: Option<Field<'a>>,
    used_anchors: std::collections::BTreeSet<usize>,
}

fn mapping_field<'a>(header: &'a str, key: &str) -> Result<MappingField<'a>> {
    let mut depth = 0;
    let mut root = None;
    let mut expecting_key = true;
    let mut matches_key = false;
    let mut field = None::<Field<'_>>;
    let mut in_sequence = false;
    let mut used_anchors = std::collections::BTreeSet::new();
    let mut scalar_anchors = std::collections::BTreeMap::new();
    for event in Parser::new_from_str(header) {
        let (event, span) = event.context("scanning work-unit front matter")?;
        if let Event::Alias(anchor) = &event {
            used_anchors.insert(*anchor);
        }
        if depth == 0
            && let Event::MappingStart(style, ..) = event
        {
            root = Some((style, span));
            depth = 1;
            continue;
        }
        if depth == 1 && event.is_node() {
            if expecting_key {
                matches_key = match &event {
                    Event::Scalar(value, ..) => value == key,
                    Event::Alias(anchor) => scalar_anchors.get(anchor) == Some(&true),
                    _ => false,
                };
            } else if matches_key {
                in_sequence = matches!(event, Event::SequenceStart(..));
                field = Some(Field {
                    start: (event.clone(), span),
                    end: span,
                    node_depth: match event {
                        Event::MappingStart(..) | Event::SequenceStart(..) => Some(depth + 1),
                        _ => None,
                    },
                    sequence_end: None,
                    last_item: None,
                });
            }
            expecting_key = !expecting_key;
        } else if in_sequence {
            let field = field.as_mut().expect("sequence field was found");
            match (&event, depth) {
                (Event::SequenceEnd, 2) => {
                    field.sequence_end = Some(span);
                    in_sequence = false;
                }
                (Event::MappingEnd | Event::SequenceEnd, 3)
                | (Event::Scalar(..) | Event::Alias(_), 2) => field.last_item = Some(span),
                _ => (),
            }
        }
        if let Some(field) = field.as_mut()
            && let Some(node_depth) = field.node_depth
            && depth == node_depth
            && matches!(event, Event::MappingEnd | Event::SequenceEnd)
        {
            field.end = span;
            field.node_depth = None;
        }
        match event {
            Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
            Event::MappingEnd | Event::SequenceEnd => depth -= 1,
            Event::Scalar(value, _, anchor, _) if anchor != 0 => {
                scalar_anchors.insert(anchor, value == key);
            }
            _ => (),
        }
    }
    Ok(MappingField {
        root: root.context("expected a front matter mapping")?,
        field,
        used_anchors,
    })
}

// Both callers supply values that are safe as single- and double-quoted YAML scalars.
fn set_scalar(header: &str, newline: &str, key: &str, value: &str, quote: bool) -> Result<String> {
    let mapping = mapping_field(header, key)?;
    let mut updated = header.to_owned();
    let literal = if quote {
        format!("'{value}'")
    } else {
        value.to_owned()
    };
    if let Some(Field {
        start: (event, span),
        ..
    }) = mapping.field
    {
        let start = span.start.byte_offset().context("missing scalar offset")?;
        let end = span.end.byte_offset().context("missing scalar end")?;
        let token = header
            .get(start..end)
            .context("invalid scalar byte range")?;
        match event {
            Event::Scalar(existing, style, anchor, _) => {
                ensure!(
                    !mapping.used_anchors.contains(&anchor),
                    "cannot change a {key} anchor referenced by another YAML field"
                );
                if token.is_empty() {
                    // The parser locates an implicit null at the mapping's colon.
                    let offset = start + usize::from(header.as_bytes().get(start) == Some(&b':'));
                    updated.insert_str(offset, &format!(" {literal}"));
                    return Ok(updated);
                }
                let replacement = match style {
                    ScalarStyle::SingleQuoted => format!("'{value}'"),
                    ScalarStyle::DoubleQuoted => format!("\"{value}\""),
                    ScalarStyle::Literal | ScalarStyle::Folded => {
                        // The parser's block scalar span covers the content,
                        // leaving the indicator and its comment outside the edit.
                        ensure!(
                            !existing.is_empty(),
                            "cannot edit an empty block scalar for {key}"
                        );
                        let offset = token
                            .find(existing.as_ref())
                            .with_context(|| format!("cannot locate block scalar for {key}"))?;
                        updated
                            .replace_range(start + offset..start + offset + existing.len(), value);
                        return Ok(updated);
                    }
                    _ => literal,
                };
                updated.replace_range(start..end, &replacement);
            }
            Event::Alias(_) => updated.replace_range(start..end, &literal),
            _ => anyhow::bail!("expected a scalar value for {key}"),
        }
    } else {
        insert_field(
            &mut updated,
            mapping.root,
            &format!("{key}: {literal}"),
            newline,
        )?;
    }
    Ok(updated)
}

fn flow_item(item: &impl Serialize) -> Result<String> {
    let item = serde_saphyr::to_string(&serde_saphyr::FlowMap(item))
        .context("serializing a work-unit sequence item")?;
    Ok(item.trim_end().to_owned())
}

fn append_items(header: &str, newline: &str, key: &str, items: &[String]) -> Result<String> {
    let mapping = mapping_field(header, key)?;
    let mut updated = header.to_owned();
    let Some(field) = mapping.field else {
        insert_field(
            &mut updated,
            mapping.root,
            &format!("{key}: [{}]", items.join(", ")),
            newline,
        )?;
        return Ok(updated);
    };

    let (Event::SequenceStart(style, anchor, _), start) = field.start else {
        anyhow::bail!("cannot append to {key} defined by a YAML alias");
    };
    ensure!(
        !mapping.used_anchors.contains(&anchor),
        "cannot append to a {key} anchor referenced by another YAML field"
    );
    let end = field
        .sequence_end
        .with_context(|| format!("missing {key} sequence end"))?;
    match style {
        StructureStyle::Flow => {
            if let Some(last) = field.last_item {
                let offset = last
                    .end
                    .byte_offset()
                    .context("missing last sequence item end")?;
                updated.insert_str(offset, &format!(", {}", items.join(", ")));
            } else {
                let offset = start
                    .end
                    .byte_offset()
                    .context("missing sequence opening offset")?;
                updated.insert_str(offset, &items.join(", "));
            }
        }
        StructureStyle::Block => {
            let start = start
                .start
                .byte_offset()
                .context("missing sequence offset")?;
            let end = end
                .start
                .byte_offset()
                .context("missing sequence end offset")?;
            let (_, indent) = line_indent(header, start)?;
            let (line, _) = line_indent(header, end)?;
            let appended: String = items
                .iter()
                .map(|item| format!("{indent}- {item}{newline}"))
                .collect();
            updated.insert_str(line, &appended);
        }
    }
    Ok(updated)
}

fn insert_field(
    header: &mut String,
    (style, span): (StructureStyle, Span),
    field: &str,
    newline: &str,
) -> Result<()> {
    let offset = span.start.byte_offset().context("missing mapping offset")?;
    match style {
        StructureStyle::Flow => header.insert_str(offset + 1, &format!("{field}, ")),
        StructureStyle::Block => {
            let (line_start, indent) = line_indent(header, offset)?;
            let field = format!("{indent}{field}{newline}");
            header.insert_str(line_start, &field);
        }
    }
    Ok(())
}

fn line_indent(source: &str, offset: usize) -> Result<(usize, &str)> {
    let prefix = source.get(..offset).context("invalid YAML offset")?;
    let line_start = prefix.rfind('\n').map_or(0, |position| position + 1);
    let indent = &prefix[line_start..];
    ensure!(
        indent.bytes().all(|byte| byte == b' '),
        "cannot locate the indentation of the YAML node"
    );
    Ok((line_start, indent))
}

fn newline(opening: &str) -> &str {
    if opening.ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn is_delimiter(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n']) == "---"
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn accepts_crlf_and_ignores_unknown_fields() {
        let source = "---\r\nschema: mcbtodo.project/v1\r\nid: 00000000-0000-4000-8000-000000000001\r\ncustom: [one, two] # comment\r\n---\r\n\r\n# Rust OS\r\n```\r\n---\r\n```\r\n";
        assert_eq!(parse(source).unwrap().0, "Rust OS");
    }

    #[test]
    fn rejects_invalid_known_fields_and_missing_delimiter() {
        assert!(parse("---\nschema: mcbtodo.project/v1\n# Project").is_err());
        let source = "---\nschema: mcbtodo.project/v1\nid: invalid\n---\n# Project";
        assert!(parse(source).is_err());
    }

    #[test]
    fn deadline_edit_preserves_document_content_and_crlf() {
        let source = "---\r\nschema: mcbtodo.project/v1\r\nid: 00000000-0000-4000-8000-000000000001\r\ncustom: keep # comment\r\ndeadline:\r\n  kind: date\r\n  value: 2026-11-25\r\ntags: [one]\r\n---\r\n\r\n# Project\r\n\r\nBody stays.\r\n";
        let deadline = Deadline::Date {
            value: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
        };
        let updated = set_deadline(source, Some(deadline)).unwrap();
        assert!(updated.contains("custom: keep # comment\r\n"));
        assert!(
            updated.contains("deadline:\r\n  {kind: date, value: \"2026-12-01\"}\r\n"),
            "{updated:?}"
        );
        assert!(updated.ends_with("# Project\r\n\r\nBody stays.\r\n"));

        let cleared = set_deadline(&updated, None).unwrap();
        assert!(cleared.contains("deadline:\r\n  null\r\n"));
        assert!(cleared.ends_with("# Project\r\n\r\nBody stays.\r\n"));
        let (_, Metadata::Project(project)) = parse(&cleared).unwrap() else {
            panic!("expected Project");
        };
        assert_eq!(project.deadline, None);
    }
}
