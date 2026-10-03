//! Chat with Work Local Agent.
//!
//! `cww` shares chosen folders with Chat with Work through MCP tools, over a
//! WebSocket it opens itself: four read-only tools, and tools that change
//! files in the folders where the user allows changes. See README.md for the
//! threat model and PROTOCOL.md for the wire protocol.

pub mod audit;
pub mod auth;
pub mod browser;
pub mod chats;
pub mod config;
pub mod control;
pub mod daemon;
pub mod error;
pub mod limits;
pub mod logging;
pub mod paths;
pub mod policy;
pub mod proxy;
pub mod reader;
pub mod roots;
pub mod sandbox;
pub mod service;
pub mod status;
pub mod tls;
pub mod tools;
pub mod trash;
pub mod tui;
pub mod tunnel;
#[cfg(windows)]
pub mod win;
pub mod writer;
