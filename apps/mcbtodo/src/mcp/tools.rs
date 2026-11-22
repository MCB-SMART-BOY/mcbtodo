use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::NaiveDate;
use chrono_tz::Tz;
use mcbtodo_application::{MAX_BODY_BYTES, MAX_TITLE_CHARS, VaultQueries, graph_diff};
use mcbtodo_domain::{Id, time::parse_date};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_workflow::{ScriptSources, WorkflowDraft};
use serde_json::{Map, Value, json};

use crate::proposal::{self, Operation};

#[derive(Clone, Copy)]
pub(super) enum Tool {
    Projects,
    Tasks,
    WorkUnits,
    Inbox,
    ProposeTriageInbox,
    Calendar,
    CaptureTask,
    CompleteWorkUnit,
    CreateTimeBlock,
    ProposeSetDeadline,
    ProposeClearDeadline,
    ProposeCreateReminder,
    PreviewGraphDiff,
    ProposeGraphDiff,
    ProposeWorkflowDraft,
}

const TOOLS: [Tool; 15] = [
    Tool::Projects,
    Tool::Tasks,
    Tool::WorkUnits,
    Tool::Inbox,
    Tool::ProposeTriageInbox,
    Tool::Calendar,
    Tool::CaptureTask,
    Tool::CompleteWorkUnit,
    Tool::CreateTimeBlock,
    Tool::ProposeSetDeadline,
    Tool::ProposeClearDeadline,
    Tool::ProposeCreateReminder,
    Tool::PreviewGraphDiff,
    Tool::ProposeGraphDiff,
    Tool::ProposeWorkflowDraft,
];

pub(super) fn list() -> Vec<Value> {
    TOOLS
        .into_iter()
        .filter(|tool| tool.available())
        .map(Tool::description)
        .collect()
}

fn graph_diff_units_schema() -> Value {
    json!({
        "type": "array", "minItems": 1, "maxItems": 32,
        "items": {
            "type": "object", "additionalProperties": false, "required": ["id", "title"],
            "properties": {
                "id": {"type": "string", "format": "uuid"},
                "title": {"type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS},
                "body": {"type": "string", "maxLength": MAX_BODY_BYTES},
                "depends_on": {
                    "type": "array", "uniqueItems": true, "maxItems": 32,
                    "items": {"type": "string", "format": "uuid"},
                    "description": "Prerequisite WorkUnit IDs from the Vault or this draft."
                },
                "estimated_minutes": {"type": "integer", "minimum": 0, "maximum": u32::MAX}
            }
        }
    })
}

impl Tool {
    pub(super) fn named(name: &str) -> Option<Self> {
        TOOLS.into_iter().find(|tool| tool.name() == name)
    }

    fn available(self) -> bool {
        proposal::QUEUE_SUPPORTED || self.proposal_kind().is_none()
    }

    fn name(self) -> &'static str {
        match self {
            Self::Projects => "list_projects",
            Self::Tasks => "list_tasks",
            Self::WorkUnits => "list_work_units",
            Self::Inbox => "list_inbox",
            Self::ProposeTriageInbox => "propose_triage_inbox",
            Self::Calendar => "calendar_day",
            Self::CaptureTask => "propose_capture_task",
            Self::CompleteWorkUnit => "propose_complete_work_unit",
            Self::CreateTimeBlock => "propose_create_time_block",
            Self::ProposeSetDeadline => "propose_set_deadline",
            Self::ProposeClearDeadline => "propose_clear_deadline",
            Self::ProposeCreateReminder => "propose_create_reminder",
            Self::PreviewGraphDiff => "preview_work_unit_graph_diff",
            Self::ProposeGraphDiff => "propose_work_unit_graph_diff",
            Self::ProposeWorkflowDraft => "propose_workflow_draft",
        }
    }

    fn argument_names(self) -> &'static [&'static str] {
        match self {
            Self::Projects | Self::Inbox => &[],
            Self::ProposeTriageInbox => {
                &["inbox_id", "project_id", "expected_revision", "request_id"]
            }
            Self::Tasks => &["project_id"],
            Self::WorkUnits => &["project_id", "task_id"],
            Self::Calendar => &["date"],
            Self::CaptureTask => &["project_id", "title", "body", "request_id"],
            Self::CompleteWorkUnit => &["work_unit_id", "request_id"],
            Self::CreateTimeBlock => &[
                "work_unit_id",
                "expected_revision",
                "start",
                "end",
                "availability",
                "request_id",
            ],
            Self::ProposeSetDeadline => &["target_id", "expected_revision", "date", "request_id"],
            Self::ProposeClearDeadline => &["target_id", "expected_revision", "request_id"],
            Self::ProposeCreateReminder => &[
                "target_id",
                "expected_revision",
                "schedule",
                "title",
                "body",
                "request_id",
            ],
            Self::PreviewGraphDiff => &["task_id", "units"],
            Self::ProposeGraphDiff => &["task_id", "units", "request_id"],
            Self::ProposeWorkflowDraft => &["graph", "sources", "request_id"],
        }
    }

    fn proposal_kind(self) -> Option<&'static str> {
        match self {
            Self::CaptureTask => Some("capture_task"),
            Self::CompleteWorkUnit => Some("complete_work_unit"),
            Self::CreateTimeBlock => Some("create_time_block"),
            Self::ProposeSetDeadline => Some("set_deadline"),
            Self::ProposeClearDeadline => Some("clear_deadline"),
            Self::ProposeCreateReminder => Some("create_reminder"),
            Self::ProposeGraphDiff => Some("work_unit_graph_diff"),
            Self::ProposeTriageInbox => Some("triage_inbox"),
            Self::ProposeWorkflowDraft => Some("publish_workflow"),
            _ => None,
        }
    }

    fn description(self) -> Value {
        let description = match self {
            Self::Projects => "List project IDs and titles in the configured Vault.",
            Self::Tasks => "List task IDs, titles, and parent project IDs.",
            Self::WorkUnits => {
                "List work unit IDs, titles, parents, current revision digests and dependency-blocked status."
            }
            Self::Inbox => {
                "List captured Inbox entries with their IDs, titles, capture times, tags, optional project assignments, and revision digests."
            }
            Self::ProposeTriageInbox => {
                "Queue an Inbox-to-Project assignment for human review. Use the Inbox and Project IDs and current Inbox revision from list_inbox and list_projects."
            }
            Self::Calendar => {
                "Read calendar entries for one date in the configured Vault timezone."
            }
            Self::CaptureTask => "Queue a task capture proposal for human review in the CLI.",
            Self::CompleteWorkUnit => {
                "Queue a work unit completion proposal for human review in the CLI."
            }
            Self::CreateTimeBlock => {
                "Propose a work unit time block for human review. Use the revision from list_work_units and timestamps with offsets matching the Vault timezone."
            }
            Self::ProposeSetDeadline => {
                "Queue a date deadline proposal for a project, task, or work unit. Use the target's revision digest from the corresponding list tool."
            }
            Self::ProposeClearDeadline => {
                "Queue a proposal to clear a project, task, or work unit deadline. Use the target's revision digest from the corresponding list tool."
            }
            Self::ProposeCreateReminder => {
                "Queue a reminder for a Task or WorkUnit for human review. The schedule must be a valid once or recurring ReminderSchedule."
            }
            Self::PreviewGraphDiff => {
                "Validate a batch of new work units under one task and preview its nodes, edges, and ready units."
            }
            Self::ProposeGraphDiff => {
                "Queue a validated batch of new work units for human review in the CLI and return its graph diff."
            }
            Self::ProposeWorkflowDraft => {
                "Capture an exact workflow graph and its source bytes for human review. Approval publishes an immutable revision but does not authorize or execute scripts. Supply sources as graph-relative paths mapped to standard Base64."
            }
        };
        let properties: Map<_, _> = self.argument_names().iter().map(|&key| {
            let schema = match key {
                "date" => json!({"type": "string", "format": "date", "description": "Date in YYYY-MM-DD form."}),
                "project_id" => json!({"type": "string", "format": "uuid", "description": "Project ID."}),
                "inbox_id" => json!({"type": "string", "format": "uuid", "description": "Inbox entry ID."}),
                "task_id" => json!({"type": "string", "format": "uuid", "description": "Parent task ID."}),
                "work_unit_id" => json!({"type": "string", "format": "uuid", "description": "Work unit ID."}),
                "target_id" => json!({"type": "string", "format": "uuid", "description": "Project, task, or work unit ID."}),
                "title" => json!({"type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS}),
                "body" => json!({"type": "string", "maxLength": MAX_BODY_BYTES, "description": "Optional task Markdown body, limited to 16 KiB in UTF-8."}),
                "request_id" => json!({"type": "string", "format": "uuid", "description": "Optional unique request UUID. Reuse it with identical input to retry submission."}),
                "expected_revision" => json!({"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Revision digest from the corresponding list_projects, list_tasks, or list_work_units row."}),
                "start" => json!({"type": "string", "format": "date-time", "description": "Inclusive RFC 3339 start."}),
                "end" => json!({"type": "string", "format": "date-time", "description": "Exclusive RFC 3339 end."}),
                "availability" => json!({"type": "string", "enum": ["busy", "free"], "default": "busy"}),
                "units" => graph_diff_units_schema(),
                "graph" => json!({"type": "string", "minLength": 1, "maxLength": 1048576, "description": "Exact UTF-8 workflow YAML bytes."}),
                "sources" => json!({
                    "type": "object",
                    "maxProperties": 64,
                    "additionalProperties": {"type": "string", "contentEncoding": "base64"},
                    "description": "Map each graph-relative script/input path to its exact bytes encoded as standard Base64."
                }),
                "schedule" => json!({
                    "type": "object",
                    "description": "ReminderSchedule object: {kind: once, at: {value: RFC3339, timezone: IANA zone}} or {kind: recurring, rule: {start, through?, time, timezone, frequency, interval?, dst: {gap, fold}}}.",
                    "oneOf": [
                        {"type": "object", "properties": {"kind": {"const": "once"}, "at": {"type": "object"}}, "required": ["kind", "at"], "additionalProperties": false},
                        {"type": "object", "properties": {"kind": {"const": "recurring"}, "rule": {"type": "object"}}, "required": ["kind", "rule"], "additionalProperties": false}
                    ]
                }),
                _ => unreachable!("open-ended tool schema"),
            };
            (key.to_owned(), schema)
        }).collect();
        let mut schema =
            json!({"type": "object", "properties": properties, "additionalProperties": false});
        let required: &[&str] = match self {
            Self::Calendar => &["date"],
            Self::CaptureTask => &["project_id", "title"],
            Self::CompleteWorkUnit => &["work_unit_id"],
            Self::CreateTimeBlock => &["work_unit_id", "expected_revision", "start", "end"],
            Self::ProposeSetDeadline => &["target_id", "expected_revision", "date"],
            Self::ProposeClearDeadline => &["target_id", "expected_revision"],
            Self::ProposeCreateReminder => &["target_id", "expected_revision", "schedule", "title"],
            Self::ProposeTriageInbox => &["inbox_id", "project_id", "expected_revision"],
            Self::PreviewGraphDiff | Self::ProposeGraphDiff => &["task_id", "units"],
            Self::ProposeWorkflowDraft => &["graph", "sources"],
            _ => &[],
        };
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        let annotations = if self.proposal_kind().is_some() {
            json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false})
        } else {
            json!({"readOnlyHint": true, "openWorldHint": false})
        };
        json!({
            "name": self.name(), "description": description, "inputSchema": schema,
            "annotations": annotations
        })
    }

    pub(super) fn run(
        self,
        root: &Path,
        timezone: Tz,
        paths: &WorkspacePaths,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        if !self.available() {
            return Err("The private proposal queue is unavailable on this platform.".into());
        }
        if arguments
            .keys()
            .any(|key| !self.argument_names().contains(&key.as_str()))
        {
            return Err("Unexpected tool argument. See the tool's input schema.".into());
        }
        if matches!(self, Self::ProposeWorkflowDraft) {
            let request_id = optional_id(arguments, "request_id")
                .ok_or("request_id must be a UUID")?
                .unwrap_or_else(Id::new_v4);
            let graph = arguments
                .get("graph")
                .and_then(Value::as_str)
                .ok_or("graph must be a UTF-8 string")?
                .to_owned();
            let encoded = arguments
                .get("sources")
                .and_then(Value::as_object)
                .ok_or("sources must be an object of Base64 strings")?;
            let mut sources = ScriptSources::new();
            for (path, value) in encoded {
                let value = value
                    .as_str()
                    .ok_or("each workflow source must be a Base64 string")?;
                let bytes = STANDARD
                    .decode(value)
                    .map_err(|_| "workflow source contains invalid Base64")?;
                sources.insert(path.clone(), bytes);
            }
            let draft = WorkflowDraft::new(graph, sources)
                .map_err(|_| "Workflow snapshot is invalid or exceeds the publication limits.")?;
            let expected_revision = draft
                .hash()
                .map_err(|_| "Could not hash workflow snapshot.")?;
            let submission = proposal::submit(
                root,
                timezone,
                paths,
                Operation::PublishWorkflow { draft },
                Some(request_id),
            )
            .map_err(|error| {
                eprintln!("mcbtodo MCP workflow proposal submission: {error:#}");
                "Could not queue the workflow proposal."
            })?;
            return Ok(json!({
                "proposal_id": submission.proposal_id,
                "status": "queued",
                "workflow_revision": expected_revision,
                "execution_authorized": false
            }));
        }
        if let Some(kind) = self.proposal_kind() {
            let request_id =
                optional_id(arguments, "request_id").ok_or("request_id must be a UUID")?;
            if matches!(self, Self::ProposeSetDeadline)
                && arguments
                    .get("date")
                    .and_then(Value::as_str)
                    .and_then(parse_day)
                    .is_none()
            {
                return Err("date must be a calendar date in YYYY-MM-DD form".into());
            }
            let mut operation = arguments.clone();
            operation.remove("request_id");
            operation.insert("kind".to_owned(), json!(kind));
            let operation: Operation = serde_json::from_value(Value::Object(operation))
                .map_err(|_| "Invalid proposal arguments. See the tool's input schema.")?;
            let submission = proposal::submit(root, timezone, paths, operation, request_id)
                .map_err(|error| {
                    eprintln!("mcbtodo MCP proposal submission: {error:#}");
                    "Could not queue the proposal. Check the target, arguments, and request ID."
                })?;
            return Ok(json!(submission));
        }
        let project = optional_id(arguments, "project_id").ok_or("project_id must be a UUID")?;
        let task = optional_id(arguments, "task_id").ok_or("task_id must be a UUID")?;
        let day = if matches!(self, Self::Calendar) {
            Some(
                arguments
                    .get("date")
                    .and_then(Value::as_str)
                    .and_then(parse_day)
                    .ok_or("date must be a calendar date in YYYY-MM-DD form")?,
            )
        } else {
            None
        };
        let queries = VaultQueries::new(root, timezone);
        if matches!(self, Self::PreviewGraphDiff) {
            let draft: graph_diff::Draft = serde_json::from_value(Value::Object(arguments.clone()))
                .map_err(|_| "Invalid graph draft. See the tool's input schema.")?;
            return queries
                .preview_work_unit_graph_diff(&draft)
                .map(|diff| json!(diff))
                .map_err(|error| error.to_string());
        }
        if matches!(self, Self::Inbox) {
            let inbox = queries.list_inbox().map_err(|error| {
                eprintln!("mcbtodo MCP Vault query: {error:#}");
                "Could not query the configured Vault. Check the server's diagnostics."
            })?;
            return Ok(json!({"inbox": inbox}));
        }
        if let Some(day) = day {
            let entries = queries
                .calendar_day(day)
                .map_err(|_| "Could not project the configured calendar.")?;
            return Ok(json!({"date": day, "timezone": timezone.name(), "entries": entries}));
        }
        match self {
            Self::Projects => queries
                .list_projects()
                .map(|projects| json!({"projects": projects}))
                .map_err(|_| "Could not query the configured Vault.".into()),
            Self::Tasks => queries
                .list_tasks(project)
                .map(|tasks| json!({"tasks": tasks}))
                .map_err(|_| "Could not query the configured Vault.".into()),
            Self::WorkUnits => queries
                .list_work_units(project, task)
                .map(|work_units| json!({"work_units": work_units}))
                .map_err(|_| "Could not query the configured Vault.".into()),
            _ => unreachable!("query tool was handled above"),
        }
    }
}

fn optional_id(arguments: &Map<String, Value>, name: &str) -> Option<Option<Id>> {
    match arguments.get(name) {
        None => Some(None),
        Some(value) => Id::parse_str(value.as_str()?).ok().map(Some),
    }
}

fn parse_day(date: &str) -> Option<NaiveDate> {
    parse_date(date).ok()
}
