//! Chat with Work Local Agent.
//!
//! `cww` shares chosen folders with Chat with Work through four read-only
//! MCP tools, over a WebSocket it opens itself. See README.md for the threat
//! model and PROTOCOL.md for the wire protocol.

pub mod audit;
pub mod auth;
pub mod config;
pub mod control;
pub mod daemon;
pub mod error;
pub mod limits;
pub mod logging;
pub mod paths;
pub mod policy;
pub mod reader;
pub mod roots;
pub mod service;
pub mod status;
pub mod tools;
pub mod tunnel;
#[cfg(windows)]
pub mod win;
