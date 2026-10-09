//! boundarycheck: verify that an agent runtime transfers MCP tool results to
//! the model provider unchanged. The binary (`src/main.rs`) is a thin CLI over
//! this library; exposing the core also lets integration and property tests
//! exercise the same implementation as the executable.

pub mod adapter;
pub mod cli;
pub mod compare;
pub mod fs_secure;
pub mod isolation;
pub mod mcp;
pub mod model;
pub mod process;
pub mod provider;
pub mod report;
pub mod runner;
pub mod scenario;
