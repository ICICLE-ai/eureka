//! Lean theorem-proving harness: an agent loop over the OpenAI Responses API,
//! Lean Beam over MCP, worktrees isolation, and Comparator verification.

pub mod agent;
pub mod beam;
pub mod deps;
pub mod lean_text;
pub mod pool;
pub mod prompt;
pub mod responses;
pub mod sandbox;
pub mod search;
pub mod tools;
pub mod trace;
pub mod verify;
