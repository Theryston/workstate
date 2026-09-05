use std::path::PathBuf;

use crate::error::Result;

use super::{BoxFuture, ProcessRequest, TerminalBackend};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxWindowSnapshot {
    pub identity: String,
    pub name: String,
    pub command: Option<String>,
    pub start_command: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub process_id: Option<u32>,
    pub is_dead: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionSnapshot {
    pub identity: String,
    pub name: String,
    pub windows: Vec<TmuxWindowSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxWindowRequest {
    pub name: String,
    pub process: ProcessRequest,
}

pub trait TmuxBackend: TerminalBackend {
    fn observe<'a>(&'a self) -> BoxFuture<'a, Result<Vec<TmuxSessionSnapshot>>>;

    fn create_session<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>>;

    fn create_window<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>>;

    fn kill_window<'a>(
        &'a self,
        session_name: &'a str,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<()>>;

    fn kill_session<'a>(&'a self, session_name: &'a str) -> BoxFuture<'a, Result<()>>;
}
