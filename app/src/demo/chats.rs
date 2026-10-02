//! The stand-in daemon's chats: a synthetic organization ("Northwind")
//! with a few chats about made-up vendors and budgets, one answer still
//! streaming, tool work with sources, Markdown with lists, tables and
//! code, a failed answer, and a tool with its own view (MCP Apps). Nothing
//! here comes from a real account.
//!
//! Questions asked in the demo get a canned answer, streamed the way the
//! daemon relays the server's: the tool work first, its progress, then the
//! answer in chunks, then "changed".

use std::time::Duration;

use serde_json::{Value, json};

/// When things happened, relative to now.
fn ago(seconds: i64) -> String {
    (jiff::Timestamp::now() - jiff::SignedDuration::from_secs(seconds)).to_string()
}

/// A time yesterday afternoon, or `days` ago.
fn days_ago(days: i64) -> String {
    let date = jiff::Zoned::now().date() - jiff::Span::new().days(days);
    date.at(15, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::system())
        .map_or_else(|_| ago(days * 86_400), |z| z.timestamp().to_string())
}

pub const USER: &str = "Alex";
pub const ACCOUNT: &str = "Northwind";
const SERVER: &str = "https://chatwithwork.com";

const RENEWAL_URL: &str = "https://drive.google.com/file/d/1AcmeRenewal2026Final/view";
const MSA_URL: &str = "https://drive.google.com/file/d/1AcmeMsa2024Signed/view";

pub const ANSWER_RENEWAL: &str = "The Acme contract renews on **March 1, 2027**, with a 60-day notice window that opens on December 31 [Acme renewal 2026.pdf](https://drive.google.com/file/d/1AcmeRenewal2026Final/view).

### What changed from last year

- **Price:** €42,000 a year, up 5% from €40,000.
- **Term:** one year instead of two, renewing automatically.
- **Support:** response times drop from 8 to 4 business hours [Acme MSA 2024.pdf](https://drive.google.com/open?id=1AcmeMsa2024Signed).

| Term | 2026 | 2027 |
|---|---|---|
| Annual fee | €40,000 | €42,000 |
| Notice period | 90 days | 60 days |
| Support response | 8 hours | 4 hours |

The renewal terms as `JSON`, for the tracker:

```json
{
  \"vendor\": \"Acme\",
  \"renews\": \"2027-03-01\",
  \"annual_fee_eur\": 42000,
  \"auto_renew\": true
}
```";

pub const ANSWER_REMINDER: &str = "Here's a reminder you can paste into Slack:

> Acme renews on March 1. If we want changes, notice goes out by December 31. Dana owns the review.

Want me to add it to the Q4 checklist?";

/// The answer streaming in chat 12, in the chunks the server sends.
pub const STREAM: [&str; 6] = [
    "Q3 spending came in at **€1.28M**, 4% under plan. The biggest gaps are:\n\n",
    "1. **Travel:** €38k under, after the offsite moved online.\n",
    "2. **Software:** €21k over, from the analytics renewal",
    ".\n3. **Contractors:** on plan.\n\n",
    "Most of the travel savings were one-off, so Q4 should land closer to plan. ",
    "Want a breakdown by team?",
];

/// The part of chat 12's answer already written when it's opened.
pub fn stream_so_far() -> String {
    STREAM[..3].concat()
}

const CANNED: [&str; 4] = [
    "Here's what I found in Drive and Slack:\n\n",
    "- **Budget owner:** Dana Kim, since July.\n",
    "- **Last review:** September 24, with no open items.\n\n",
    "Want me to draft the next review's agenda?",
];

fn summary(number: u64, title: &str, state: &str, updated_at: String) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": state,
        "project": null,
        "mine": true,
        "updated_at": updated_at,
        "url": format!("{SERVER}/northwind/chats/{number}"),
    })
}

fn user(id: u64, content: &str) -> Value {
    json!({ "kind": "user", "id": id, "content": content })
}

fn assistant(id: u64, content: &str, sources: Value) -> Value {
    json!({ "kind": "assistant", "id": id, "content": content, "sources": sources })
}

fn step(summary: &str, files: &[&str]) -> Value {
    json!({ "summary": summary, "pending": false, "files": files })
}

/// A chat: its summary and its transcript's entries.
pub struct Chat {
    pub summary: Value,
    pub entries: Vec<Value>,
}

pub fn sample() -> Vec<Chat> {
    vec![
        Chat {
            summary: summary(12, "Q3 budget review", "processing", ago(60)),
            entries: vec![
                user(121, "How did Q3 spending compare with the plan?"),
                json!({
                    "kind": "activity", "id": 122,
                    "title": "Searching Drive and Slack", "details": "2 searches",
                    "progress": "Reading Q3 actuals.xlsx",
                    "services": ["Drive", "Slack"], "pending": true,
                    "steps": [
                        step("Searched Drive for “Q3 budget”", &["Q3 actuals.xlsx", "Q3 plan.pdf", "Budget 2026.xlsx"]),
                        step("Searched Slack for “Q3 travel”", &[]),
                        { "summary": "Reading Q3 actuals.xlsx…", "pending": true, "files": [] },
                    ],
                }),
                assistant(123, "", json!([])),
            ],
        },
        Chat {
            summary: summary(11, "Vendor contract renewal", "idle", ago(120)),
            entries: vec![
                user(
                    111,
                    "When does the Acme contract renew, and what changed from last year?",
                ),
                json!({
                    "kind": "activity", "id": 112,
                    "title": "Searched Drive and Slack", "details": "3 searches, read 2 files",
                    "services": ["Drive", "Slack"], "pending": false,
                    "steps": [
                        step("Searched Drive for “Acme contract”", &["Acme renewal 2026.pdf", "Acme MSA 2024.pdf"]),
                        step("Searched Slack for “Acme renewal”", &[]),
                        step("Searched Drive for “Acme support terms”", &["Acme MSA 2024.pdf"]),
                        step("Read Acme renewal 2026.pdf", &[]),
                        step("Read Acme MSA 2024.pdf", &[]),
                    ],
                }),
                assistant(
                    113,
                    ANSWER_RENEWAL,
                    json!([
                        { "title": "Acme renewal 2026.pdf", "url": RENEWAL_URL },
                        { "title": "Acme MSA 2024.pdf", "url": MSA_URL },
                    ]),
                ),
                user(114, "Draft a short reminder for the team."),
                assistant(115, ANSWER_REMINDER, json!([])),
            ],
        },
        Chat {
            summary: summary(10, "Falcon launch checklist", "idle", days_ago(1)),
            entries: vec![
                user(101, "What's left on the Falcon launch board?"),
                json!({
                    "kind": "activity", "id": 102,
                    "title": "Asked Linear", "details": "1 search",
                    "services": ["Linear"], "pending": false,
                    "steps": [{
                        "summary": "Searched Linear for “Falcon launch”", "pending": false, "files": [],
                        "app": { "service": "Linear", "uri": "ui://linear/board/falcon" },
                    }],
                }),
                assistant(
                    103,
                    "Three things are still open:\n\n1. **Pricing page copy**, with Dana, due Friday.\n2. **Status page**, waiting on the new domain.\n3. **Launch email**, drafted and in review.\n\nEverything else is done.",
                    json!([]),
                ),
            ],
        },
        Chat {
            summary: summary(9, "Onboarding plan for new hires", "error", days_ago(1)),
            entries: vec![
                user(
                    91,
                    "Put together a two-week onboarding plan for the new support hires.",
                ),
                json!({ "kind": "notice", "tone": "negative", "text": "The model didn't answer. Try again in a moment." }),
            ],
        },
        Chat {
            summary: summary(8, "Competitor pricing notes", "idle", days_ago(3)),
            entries: vec![
                user(81, "Summarize our notes on competitor pricing."),
                assistant(
                    82,
                    "Most competitors price **per seat**, between €15 and €30 a month. Two offer a free tier for up to three people.",
                    json!([]),
                ),
            ],
        },
        Chat {
            summary: summary(7, "Weekly sync summary", "idle", days_ago(6)),
            entries: vec![
                user(71, "What did we decide in this week's sync?"),
                assistant(
                    72,
                    "Ship the onboarding emails on Monday, and move the pricing review to next week.",
                    json!([]),
                ),
            ],
        },
        Chat {
            summary: summary(6, "Hiring pipeline status", "idle", days_ago(12)),
            entries: vec![
                user(61, "Where are we with the support hires?"),
                assistant(
                    62,
                    "Two offers are out and one is accepted. Interviews for the third role finish on Thursday.",
                    json!([]),
                ),
            ],
        },
    ]
}

/// A canned answer to a question asked in the demo: the steps of its
/// stream, each after a pause.
pub fn canned_stream(activity: u64, answer: u64) -> Vec<(Duration, Step)> {
    let ms = Duration::from_millis;
    let mut steps = vec![
        (
            // Long enough to see "Thinking" shimmer, as a model takes.
            ms(2500),
            Step::Activity {
                id: activity,
                pending: true,
                progress: Some("Searching Drive for “budget owner”"),
            },
        ),
        (ms(500), Step::Progress("Reading Budget 2026.xlsx")),
        (
            ms(700),
            Step::Activity {
                id: activity,
                pending: false,
                progress: None,
            },
        ),
    ];
    for chunk in CANNED {
        steps.push((
            ms(350),
            Step::Chunk {
                id: answer,
                text: chunk,
            },
        ));
    }
    steps.push((
        ms(300),
        Step::Finish {
            id: answer,
            content: CANNED.concat(),
        },
    ));
    steps
}

/// The rest of chat 12's answer, after what was written before it opened.
pub fn rest_of_stream() -> Vec<(Duration, Step)> {
    let ms = Duration::from_millis;
    let mut steps = vec![(
        ms(900),
        Step::Activity {
            id: 122,
            pending: false,
            progress: None,
        },
    )];
    for chunk in &STREAM[3..] {
        steps.push((
            ms(600),
            Step::Chunk {
                id: 123,
                text: chunk,
            },
        ));
    }
    steps.push((
        ms(500),
        Step::Finish {
            id: 123,
            content: STREAM.concat(),
        },
    ));
    steps
}

/// One thing that happens in a streamed answer.
#[derive(Debug, Clone)]
pub enum Step {
    /// The tool work starts or settles.
    Activity {
        id: u64,
        pending: bool,
        progress: Option<&'static str>,
    },
    Progress(&'static str),
    Chunk {
        id: u64,
        text: &'static str,
    },
    /// The answer is written: the chat has its text and is idle.
    Finish {
        id: u64,
        content: String,
    },
}

/// A title for a chat started with `question`, as the server names one.
pub fn title_for(question: &str) -> String {
    let title: String = question.chars().take(40).collect();
    if question.chars().count() > 40 {
        format!("{}…", title.trim_end())
    } else {
        title
    }
}
