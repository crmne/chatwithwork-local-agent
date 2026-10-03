//! The MCP server: four read-only tools, and the change tools when a shared
//! folder allows changes.
//!
//! `roots`, `search`, `list` and `read` are annotated `readOnlyHint: true`
//! and `openWorldHint: false`, with no write, exec, or network code behind
//! them. `create`, `write`, `edit`, `mkdir`, `move` and `delete` are listed
//! only while at least one folder allows changes (the user's choice, made on
//! this computer), with `create_document` for Word and Excel files; they are
//! annotated `readOnlyHint: false`, with
//! `destructiveHint: true` for the ones that can replace or remove
//! something, and run in the writer (see `crate::writer`). Every call
//! passes, in order: pause check, rate limit, argument validation, then the
//! reader or the writer (path parsing, root lookup, deny list, safe open),
//! and is appended to the audit log whatever the outcome.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CompleteRequestMethod,
    CompleteRequestParams, CompleteResult, ContentBlock, Implementation, JsonObject,
    ListPromptsRequestMethod, ListPromptsResult, ListResourceTemplatesRequestMethod,
    ListResourceTemplatesResult, ListResourcesRequestMethod, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::audit::{AuditEntry, AuditLog, Decision};
use crate::error::{ErrorCode, ToolError};
use crate::limits::Limiter;
use crate::reader::{
    ListRequest, ListResponse, ReadRequest, ReadResponse, Reader, SearchRequest, SearchResponse,
};
use crate::writer::{
    ChangeResult, CreateRequest, DeleteRequest, DocumentRequest, EditRequest, MkdirRequest,
    MoveRequest, WriteRequest, Writer,
};

/// Request `_meta` key carrying the Chat with Work chat ID, used for the
/// per-chat read budget and the audit log.
pub const CHAT_ID_META: &str = "com.chatwithwork/chatId";

/// Tool `_meta` key: the call is a plain (non-destructive) write when every
/// argument named here has the value given. A missing argument counts as
/// its default. See PROTOCOL.md.
pub const WRITE_WHEN_META: &str = "com.chatwithwork/writeWhen";

pub const READ_TOOLS: [&str; 4] = ["roots", "search", "list", "read"];

/// Offered only while a shared folder allows changes.
pub const CHANGE_TOOLS: [&str; 7] = [
    "create",
    "write",
    "edit",
    "mkdir",
    "move",
    "delete",
    "create_document",
];

/// Headroom kept below the message cap for the JSON-RPC envelope.
const ENVELOPE_HEADROOM: usize = 2048;

#[derive(Clone)]
pub struct LocalFiles {
    inner: Arc<Inner>,
}

struct Inner {
    reader: Arc<Reader>,
    writer: Arc<Writer>,
    limiter: Arc<Limiter>,
    audit: Arc<AuditLog>,
    paused: Arc<AtomicBool>,
    max_message_bytes: usize,
}

impl LocalFiles {
    pub fn new(
        reader: Arc<Reader>,
        writer: Arc<Writer>,
        limiter: Arc<Limiter>,
        audit: Arc<AuditLog>,
        paused: Arc<AtomicBool>,
        max_message_bytes: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                reader,
                writer,
                limiter,
                audit,
                paused,
                max_message_bytes,
            }),
        }
    }

    /// Run one tool call. Returns `None` for an unknown tool name.
    pub async fn call(
        &self,
        name: &str,
        args: Option<JsonObject>,
        chat_id: Option<String>,
        request_id: String,
    ) -> Option<CallToolResult> {
        let known = READ_TOOLS.contains(&name)
            || (CHANGE_TOOLS.contains(&name) && self.inner.writer.enabled());
        if !known {
            let mut entry = AuditEntry::event("tool");
            entry.tool = Some(
                name.chars()
                    .take(64)
                    .map(|c| {
                        if crate::reader::safe_fs::is_unsafe_in_path(c) {
                            '?'
                        } else {
                            c
                        }
                    })
                    .collect(),
            );
            entry.chat_id = chat_id;
            entry.request_id = Some(request_id);
            entry.decision = Some(Decision::Denied);
            entry.code = Some("unknown_tool".into());
            self.inner.audit.append(&entry);
            return None;
        }
        let args = Value::Object(args.unwrap_or_default());
        let mut entry = AuditEntry::event("tool");
        entry.tool = Some(name.to_string());
        entry.chat_id = chat_id.clone();
        entry.request_id = Some(request_id);
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .map(|s| s.chars().take(512).collect::<String>())
        };
        entry.path = text("path").or_else(|| text("from"));
        entry.to = text("to");
        if args.get("dry_run") == Some(&Value::Bool(true)) {
            entry.dry_run = Some(true);
        }
        entry.query = args
            .get("query")
            .and_then(Value::as_str)
            .map(|s| s.chars().take(256).collect());

        let started = std::time::Instant::now();
        let outcome = self.run(name, args, chat_id.as_deref().unwrap_or("")).await;
        let result = match outcome {
            Ok(output) => {
                if let Output::Change(change) = &output {
                    entry.effect = Some(change.effect.as_str().into());
                    entry.written = (change.written > 0).then_some(change.written);
                    entry.trash = change.trashed_to.as_ref().map(|p| p.display().to_string());
                    if change.from.is_some() {
                        entry.path = change.from.clone();
                        entry.to = Some(change.path.clone());
                    }
                }
                let (result, bytes, results) = output.into_result(self.budget());
                entry.decision = Some(Decision::Allowed);
                entry.bytes = Some(bytes);
                entry.results = results;
                result
            }
            Err(err) => {
                entry.decision = Some(if err.code.is_denial() {
                    Decision::Denied
                } else {
                    Decision::Error
                });
                entry.code = Some(err.code.as_str().into());
                entry.reason = Some(err.message.clone());
                error_result(&err)
            }
        };
        entry.duration_ms = Some((started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0);
        self.inner.audit.append(&entry);
        Some(result)
    }

    fn budget(&self) -> usize {
        self.inner
            .max_message_bytes
            .saturating_sub(ENVELOPE_HEADROOM)
    }

    async fn run(&self, name: &str, args: Value, chat: &str) -> Result<Output, ToolError> {
        if self.inner.paused.load(Ordering::SeqCst) {
            return Err(ToolError::new(
                ErrorCode::Paused,
                "the user paused access to this computer",
            ));
        }
        self.inner.limiter.check_call()?;
        let reader = Arc::clone(&self.inner.reader);
        match name {
            "roots" => {
                let _: EmptyArgs = parse_args(args)?;
                let roots = blocking(move || Ok(reader.roots())).await?;
                Ok(Output::Roots(json!({ "roots": roots })))
            }
            "search" => {
                let req: SearchRequest = parse_args(args)?;
                let resp = blocking(move || reader.search(&req)).await?;
                Ok(Output::Search(resp))
            }
            "list" => {
                let req: ListRequest = parse_args(args)?;
                let start = req
                    .cursor
                    .as_deref()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                let resp = blocking(move || reader.list(&req)).await?;
                Ok(Output::List(resp, start))
            }
            "read" => {
                let mut req: ReadRequest = parse_args(args)?;
                let allowance = self.inner.limiter.read_allowance(chat)?;
                req.max_chars = Some(req.max_chars.unwrap_or(allowance).min(allowance));
                let mut resp = blocking(move || reader.read(&req)).await?;
                shrink_read_to_fit(&mut resp, self.budget());
                self.inner
                    .limiter
                    .record_read(chat, resp.text.chars().count());
                Ok(Output::Read(resp))
            }
            change if CHANGE_TOOLS.contains(&change) => {
                // A dry run of `write` or `edit` answers with a diff of the
                // file as it is: that is a read, and counts against the
                // read budget like one.
                let shows_content = matches!(change, "write" | "edit")
                    && args.get("dry_run") == Some(&Value::Bool(true));
                let allowance = if shows_content {
                    Some(self.inner.limiter.read_allowance(chat)?)
                } else {
                    None
                };
                let writer = Arc::clone(&self.inner.writer);
                let name = change.to_string();
                let mut result = blocking(move || run_change(&writer, &name, args)).await?;
                if let (Some(allowance), Some(diff)) = (allowance, result.diff.as_mut()) {
                    cut_to_chars(diff, allowance);
                    self.inner.limiter.record_read(chat, diff.chars().count());
                }
                Ok(Output::Change(result))
            }
            _ => unreachable!("checked against the tool names"),
        }
    }

    /// The tools offered right now.
    pub fn tools(&self) -> Vec<Tool> {
        tools(self.inner.writer.enabled())
    }
}

/// Run the change tool `name` with `args` in `writer`.
pub fn run_change(writer: &Writer, name: &str, args: Value) -> Result<ChangeResult, ToolError> {
    match name {
        "create" => writer.create(&parse_args::<CreateRequest>(args)?),
        "write" => writer.write(&parse_args::<WriteRequest>(args)?),
        "edit" => writer.edit(&parse_args::<EditRequest>(args)?),
        "mkdir" => writer.mkdir(&parse_args::<MkdirRequest>(args)?),
        "move" => writer.move_entry(&parse_args::<MoveRequest>(args)?),
        "delete" => writer.delete(&parse_args::<DeleteRequest>(args)?),
        "create_document" => writer.create_document(&parse_args::<DocumentRequest>(args)?),
        other => Err(ToolError::invalid_argument(format!(
            "{other:?} is not a change tool"
        ))),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

enum Output {
    Roots(Value),
    Search(SearchResponse),
    /// The page, and the offset it starts at.
    List(ListResponse, usize),
    Read(ReadResponse),
    Change(ChangeResult),
}

impl Output {
    /// Build the tool result, dropping trailing items until it fits
    /// `budget` bytes. Returns the result, its size, and the item count.
    fn into_result(self, budget: usize) -> (CallToolResult, usize, Option<usize>) {
        match self {
            Output::Roots(v) => {
                let n = v["roots"].as_array().map(Vec::len);
                let (r, b) = success(&v);
                (r, b, n)
            }
            Output::Search(mut resp) => loop {
                let (r, b) = success(&resp);
                if b <= budget || resp.hits.is_empty() {
                    return (r, b, Some(resp.hits.len()));
                }
                resp.hits.pop();
            },
            Output::List(mut resp, start) => loop {
                let (r, b) = success(&resp);
                if b <= budget || resp.entries.len() <= 1 {
                    return (r, b, Some(resp.entries.len()));
                }
                let keep = resp.entries.len() / 2;
                resp.entries.truncate(keep);
                resp.next_cursor = Some((start + keep).to_string());
            },
            Output::Read(resp) => {
                let (r, b) = success(&resp);
                (r, b, None)
            }
            Output::Change(resp) => {
                let (r, b) = success(&resp);
                (r, b, None)
            }
        }
    }
}

/// Cut `text` to at most `max` characters, saying so at the end.
fn cut_to_chars(text: &mut String, max: usize) {
    const MORE: &str = "\n… (the read budget ends here)\n";
    if text.chars().count() <= max {
        return;
    }
    let keep = max.saturating_sub(MORE.chars().count());
    let end = text.char_indices().nth(keep).map_or(text.len(), |(i, _)| i);
    text.truncate(end);
    if max >= MORE.chars().count() {
        text.push_str(MORE);
    }
}

/// Halve the chunk until the serialized result fits the message budget.
fn shrink_read_to_fit(resp: &mut ReadResponse, budget: usize) {
    loop {
        let size = result_size(resp);
        let chars = resp.text.chars().count();
        if size <= budget || chars <= 1 {
            return;
        }
        let keep = chars / 2;
        resp.text = resp.text.chars().take(keep).collect();
        resp.next_offset = Some(resp.offset + keep);
    }
}

fn result_size<T: Serialize>(value: &T) -> usize {
    success(value).1
}

/// A successful result: the JSON as `structuredContent` and, for clients that
/// only read content blocks, the same JSON as text.
fn success<T: Serialize>(value: &T) -> (CallToolResult, usize) {
    let structured = serde_json::to_value(value).unwrap_or(Value::Null);
    let text = structured.to_string();
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(structured);
    let size = serde_json::to_vec(&result).map_or(usize::MAX, |v| v.len());
    (result, size)
}

pub fn error_result(err: &ToolError) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(format!(
        "{}: {}",
        err.code.as_str(),
        err.message
    ))]);
    result.structured_content = Some(json!({
        "error": { "code": err.code.as_str(), "message": err.message }
    }));
    result
}

fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args)
        .map_err(|e| ToolError::invalid_argument(format!("invalid arguments: {e}")))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(ToolError::internal("the reader crashed on this request")))
}

fn schema(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!("schemas are objects"),
    }
}

fn tool(name: &'static str, title: &str, description: &'static str, input: Value) -> Tool {
    Tool::new(
        Cow::Borrowed(name),
        Cow::Borrowed(description),
        schema(input),
    )
    .with_title(title)
    .annotate(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

/// A tool that changes files: never read-only, never open-world.
fn change_tool(
    name: &'static str,
    title: &str,
    description: &'static str,
    input: Value,
    destructive: bool,
    idempotent: bool,
    write_when: Option<Value>,
) -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(name),
        Cow::Borrowed(description),
        schema(input),
    )
    .with_title(title)
    .annotate(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(destructive)
            .idempotent(idempotent)
            .open_world(false),
    );
    if let Some(when) = write_when {
        let mut meta = JsonObject::new();
        meta.insert(WRITE_WHEN_META.into(), when);
        tool = tool.with_meta(rmcp::model::MetaObject(meta));
    }
    tool
}

const DRY_RUN: &str = "Check the change and describe it without making it.";

/// The change tools, in the order they are listed.
pub fn change_tools() -> Vec<Tool> {
    vec![
        change_tool(
            "create",
            "Create a local file",
            "Create a new text file in a shared folder that allows changes (`writable: true` in \
             `roots`). Fails if anything is already at the path, and the folder it goes in \
             must exist (see `mkdir`). For Word and Excel files use `create_document`. \
             Programs, scripts that run when opened, and shortcuts are never created. The \
             person approves every change.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` of the new file." },
                    "content": { "type": "string", "description": "The file's text (UTF-8)." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            false,
            false,
            None,
        ),
        change_tool(
            "write",
            "Write a local file",
            "Replace a text file's whole content (`mode: \"replace\"`, the default) or add text \
             at its end (`mode: \"append\"`), in a shared folder that allows changes. Creates \
             the file if it doesn't exist. The previous version goes to the system trash on \
             the person's computer. Office documents, PDFs and other binary files can't be \
             written as text. `expected_sha256` makes the call fail if the file's current \
             SHA-256 (as a dry run reports it) is different.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` of the file." },
                    "content": { "type": "string", "description": "The new text, or the text to add." },
                    "mode": { "type": "string", "enum": ["replace", "append"], "description": "Default `replace`." },
                    "expected_sha256": { "type": "string", "description": "Only change the file if its current content has this SHA-256 (hex)." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            true,
            false,
            Some(json!({ "mode": "append" })),
        ),
        change_tool(
            "edit",
            "Edit a local file",
            "Replace one exact span of a text file in a shared folder that allows changes: \
             `old_text` must appear exactly once in the file (quote it exactly, with enough \
             surrounding text to be unique) and becomes `new_text`. It changes a part of a \
             file: at most 8 KiB and at most half the file (any span of up to 64 bytes); use \
             `write` to replace more. The previous version goes to the system trash on the \
             person's computer.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` of the file." },
                    "old_text": { "type": "string", "description": "The exact text to replace; it must appear once." },
                    "new_text": { "type": "string", "description": "What replaces it." },
                    "expected_sha256": { "type": "string", "description": "Only change the file if its current content has this SHA-256 (hex)." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path", "old_text", "new_text"],
                "additionalProperties": false
            }),
            false,
            false,
            None,
        ),
        change_tool(
            "mkdir",
            "Create a local folder",
            "Create a folder in a shared folder that allows changes. With `parents: true`, also \
             create the missing folders on the way. A folder that already exists is left as \
             it is.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` of the folder." },
                    "parents": { "type": "boolean", "description": "Create missing parent folders too." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            false,
            true,
            None,
        ),
        change_tool(
            "move",
            "Move a local file",
            "Move or rename a file or folder, within a shared folder or between two that allow \
             changes. Fails if something is already at `to`, unless `replace: true`, which \
             moves that file to the system trash on the person's computer first (a file can \
             only replace a file).",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "`<root_id>:relative/path` to move." },
                    "to": { "type": "string", "description": "`<root_id>:relative/path` it moves to, name included." },
                    "replace": { "type": "boolean", "description": "Move a file already at `to` to the trash first. Default false." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
            true,
            false,
            Some(json!({ "replace": false })),
        ),
        change_tool(
            "delete",
            "Delete a local file",
            "Move a file or folder in a shared folder that allows changes to the system trash \
             on the person's computer (the Trash on macOS, the Recycle Bin on Windows, the \
             desktop's trash on Linux), where they can restore it. Nothing is deleted for \
             good.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` to move to the trash." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            true,
            false,
            None,
        ),
        change_tool(
            "create_document",
            "Create a Word or Excel file",
            "Create a Word document (path ending in `.docx`) from Markdown in `content`: \
             headings, paragraphs, bold, italic, lists, quotes, code blocks and tables; links \
             become their text with the address after it. Or an Excel workbook (`.xlsx`) from \
             `sheets`, each a `name` and `rows` of cells (text, numbers, true or false, null \
             for empty; never formulas), with `header: true` to bold and freeze the first \
             row. Fails if the file exists, unless `replace: true`, which moves the current \
             file to the system trash on the person's computer and writes a new one: \
             existing documents are never edited in place.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path` ending in .docx or .xlsx." },
                    "content": { "type": "string", "description": "For .docx: the document in Markdown." },
                    "sheets": {
                        "type": "array",
                        "description": "For .xlsx: the sheets, in order.",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "1 to 31 characters, none of [ ] : * ? / \\." },
                                "rows": {
                                    "type": "array",
                                    "items": { "type": "array", "items": { "type": ["string", "number", "boolean", "null"] } }
                                },
                                "header": { "type": "boolean", "description": "Bold and freeze the first row." }
                            },
                            "required": ["name", "rows"],
                            "additionalProperties": false
                        }
                    },
                    "replace": { "type": "boolean", "description": "Move an existing file at the path to the trash first. Default false." },
                    "dry_run": { "type": "boolean", "description": DRY_RUN }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            true,
            false,
            Some(json!({ "replace": false })),
        ),
    ]
}

/// The tools to offer: the read-only four, then the change tools when
/// `changes` (a shared folder allows them).
pub fn tools(changes: bool) -> Vec<Tool> {
    let mut all = read_tools();
    if changes {
        all.extend(change_tools());
    }
    all
}

fn read_tools() -> Vec<Tool> {
    vec![
        tool(
            "roots",
            "Shared folders",
            "List the folders the user shared from this computer. Returns each folder's ID and \
             label. Paths in the other tools look like `<root_id>:relative/path`.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            "search",
            "Search local files",
            "Full-text search across the shared folders on the user's computer. Returns matching \
             files with a snippet each. `path_glob` without a slash matches file names (`*.pdf`); \
             with a slash it matches relative paths (`plans/**/*.md`).",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to search for." },
                    "root": { "type": "string", "description": "Only search this root ID." },
                    "path_glob": { "type": "string", "description": "Only files matching this glob." },
                    "modified_after": { "type": "string", "description": "RFC 3339 time or YYYY-MM-DD." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum hits (default 20)." }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "list",
            "List a local folder",
            "List the entries of a folder on the user's computer. Use `<root_id>:` for the top \
             of a shared folder or `<root_id>:relative/dir`. Pass `next_cursor` back as `cursor` \
             for the next page.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/dir`" },
                    "cursor": { "type": "string", "description": "Cursor from a previous page." }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        tool(
            "read",
            "Read a local file",
            "Read the text of a file on the user's computer. PDF, Word, PowerPoint and Excel \
             files are converted to text. Returns up to `max_chars` characters from `offset`, \
             plus `next_offset` for the next chunk (null at the end).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "`<root_id>:relative/path`" },
                    "offset": { "type": "integer", "minimum": 0, "description": "Character offset (default 0)." },
                    "max_chars": { "type": "integer", "minimum": 1, "maximum": 32000, "description": "Characters to return (default and maximum 32000)." }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
    ]
}

impl ServerHandler for LocalFiles {
    fn get_info(&self) -> ServerConfig {
        let instructions = if self.inner.writer.enabled() {
            "Access to folders the user shared from their computer. Call `roots` first. Paths \
             are always `<root_id>:relative/path`; absolute paths are refused. Folders with \
             `writable: true` also take changes (`create`, `write`, `edit`, `mkdir`, `move`, \
             `delete`, `create_document`); the others are read-only. Old versions and deleted \
             files go to the system trash on the computer."
        } else {
            "Read-only access to folders the user shared from their computer. Call `roots` \
             first. Paths are always `<root_id>:relative/path`; absolute paths are refused."
        };
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(instructions);
        info.server_info = Implementation::new("cww", env!("CARGO_PKG_VERSION"));
        info
    }

    // Only tools are served. rmcp answers these with empty lists by default;
    // refuse them instead so the surface is exactly the four tools.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Err(McpError::method_not_found::<ListPromptsRequestMethod>())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Err(McpError::method_not_found::<ListResourcesRequestMethod>())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Err(McpError::method_not_found::<
            ListResourceTemplatesRequestMethod,
        >())
    }

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Err(McpError::method_not_found::<CompleteRequestMethod>())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let chat_id = request
            .meta
            .as_ref()
            .and_then(|m| m.get(CHAT_ID_META))
            .or_else(|| context.meta.get(CHAT_ID_META))
            .and_then(|v| match v {
                Value::String(s) => Some(s.chars().take(128).collect()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            });
        let request_id = match serde_json::to_value(&context.id) {
            Ok(Value::String(s)) => s,
            Ok(other) => other.to_string(),
            Err(_) => String::new(),
        };
        match self
            .call(&request.name, request.arguments, chat_id, request_id)
            .await
        {
            Some(result) => Ok(result.into()),
            None => Err(McpError::invalid_params(
                format!(
                    "unknown tool: {}",
                    request.name.chars().take(64).collect::<String>()
                ),
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_text_to_a_number_of_characters() {
        let mut text = "é".repeat(100);
        cut_to_chars(&mut text, 60);
        assert_eq!(text.chars().count(), 60);
        assert!(text.ends_with("(the read budget ends here)\n"));
        let mut short = "abc".to_string();
        cut_to_chars(&mut short, 60);
        assert_eq!(short, "abc");
        let mut tiny = "abcdef".to_string();
        cut_to_chars(&mut tiny, 2);
        assert!(tiny.chars().count() <= 2);
    }
}
