//! The MCP server: exactly four read-only tools, and nothing else.
//!
//! `roots`, `search`, `list` and `read`, all annotated `readOnlyHint: true`
//! and `openWorldHint: false`. There is no write, exec, or network code
//! behind any of them. Every call passes, in order: pause check, rate limit,
//! argument validation, then the reader (path parsing, root lookup, deny
//! list, safe open), and is appended to the audit log whatever the outcome.

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

/// Request `_meta` key carrying the Chat with Work chat ID, used for the
/// per-chat read budget and the audit log.
pub const CHAT_ID_META: &str = "com.chatwithwork/chatId";

pub const TOOL_NAMES: [&str; 4] = ["roots", "search", "list", "read"];

/// Headroom kept below the message cap for the JSON-RPC envelope.
const ENVELOPE_HEADROOM: usize = 2048;

#[derive(Clone)]
pub struct LocalFiles {
    inner: Arc<Inner>,
}

struct Inner {
    reader: Arc<Reader>,
    limiter: Arc<Limiter>,
    audit: Arc<AuditLog>,
    paused: Arc<AtomicBool>,
    max_message_bytes: usize,
}

impl LocalFiles {
    pub fn new(
        reader: Arc<Reader>,
        limiter: Arc<Limiter>,
        audit: Arc<AuditLog>,
        paused: Arc<AtomicBool>,
        max_message_bytes: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                reader,
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
        if !TOOL_NAMES.contains(&name) {
            let mut entry = AuditEntry::event("tool");
            entry.tool = Some(name.chars().take(64).collect());
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
        entry.path = args
            .get("path")
            .and_then(Value::as_str)
            .map(|s| s.chars().take(512).collect());
        entry.query = args
            .get("query")
            .and_then(Value::as_str)
            .map(|s| s.chars().take(256).collect());

        let outcome = self.run(name, args, chat_id.as_deref().unwrap_or("")).await;
        let result = match outcome {
            Ok(output) => {
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
            _ => unreachable!("checked against TOOL_NAMES"),
        }
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
        }
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

pub fn tools() -> Vec<Tool> {
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
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Read-only access to folders the user shared from their computer. Call `roots` \
                 first. Paths are always `<root_id>:relative/path`; absolute paths are refused.",
            );
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
        Ok(ListToolsResult::with_all_items(tools()))
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
