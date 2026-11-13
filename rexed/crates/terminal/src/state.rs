use crate::session::{OutputSubscription, Session};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::{CommandSpec, SessionDetail, SessionSummary, TermSize};

pub struct ServerState {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    completed: Mutex<HashMap<String, (Instant, SessionDetail)>>,
    default_command: CommandSpec,
    default_size: TermSize,
}

impl ServerState {
    pub fn new(default_command: CommandSpec, default_size: TermSize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            default_command,
            default_size,
        }
    }

    pub fn get_or_create_session(&self, name: &str, cols: u16, rows: u16) -> Result<Arc<Session>> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(name).cloned() {
            return Ok(session);
        }

        let cols = if cols == 0 {
            self.default_size.cols
        } else {
            cols
        };
        let rows = if rows == 0 {
            self.default_size.rows
        } else {
            rows
        };
        let session = Session::new(name.to_string(), self.default_command.clone(), cols, rows)?;
        sessions.insert(name.to_string(), session.clone());
        Ok(session)
    }

    pub fn create_session(
        &self,
        name: String,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<Arc<Session>> {
        Ok(self
            .create_session_with_output(name, command, cols, rows)?
            .0)
    }
    pub fn create_session_with_output(
        &self,
        name: String,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<(Arc<Session>, OutputSubscription)> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.contains_key(&name) {
            return Err(anyhow!("pty {name} already exists"));
        }

        let cols = cols.unwrap_or(self.default_size.cols);
        let rows = rows.unwrap_or(self.default_size.rows);
        let (session, output) = Session::new_with_output(name.clone(), command, cols, rows)?;
        sessions.insert(name, session.clone());
        Ok((session, output))
    }

    pub fn session(&self, name: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(name).cloned()
    }

    pub fn require_session(&self, name: &str) -> Result<Arc<Session>> {
        self.session(name)
            .ok_or_else(|| anyhow!("pty {name} does not exist"))
    }

    pub fn all_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    pub fn summaries(&self) -> Vec<SessionSummary> {
        let mut summaries = self
            .all_sessions()
            .into_iter()
            .map(|session| session.summary())
            .collect::<Vec<_>>();
        summaries.sort_by(|a, b| a.pty.cmp(&b.pty));
        summaries
    }

    pub fn detail(&self, name: &str) -> Result<SessionDetail> {
        if let Some(session) = self.session(name) {
            return Ok(session.detail());
        }
        self.completed
            .lock()
            .unwrap()
            .get(name)
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, detail)| detail.clone())
            .ok_or_else(|| anyhow!("pty {name} does not exist"))
    }

    /// Retire native resources; only a bounded, expiring final receipt remains.
    pub fn retire_session(
        &self,
        name: &str,
        session: &Arc<Session>,
        until: Instant,
        limit: usize,
    ) -> Vec<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if !sessions
            .get(name)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            return Vec::new();
        }
        let mut detail = session.detail();
        if detail.exit_code.is_none() {
            return Vec::new();
        }
        detail.env.clear();
        let mut completed = self.completed.lock().unwrap();
        sessions.remove(name);
        completed.insert(name.to_owned(), (until, detail));
        let mut removed = Vec::new();
        while completed.len() > limit {
            let oldest = completed
                .iter()
                .min_by_key(|(_, (until, _))| *until)
                .unwrap()
                .0
                .clone();
            completed.remove(&oldest);
            removed.push(oldest);
        }
        removed
    }

    pub fn prune_completed(&self, now: Instant) -> Vec<String> {
        let mut completed = self.completed.lock().unwrap();
        let expired: Vec<_> = completed
            .iter()
            .filter(|(_, (until, _))| *until <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            completed.remove(id);
        }
        expired
    }

    pub fn remove_completed(&self, name: &str) -> bool {
        self.completed.lock().unwrap().remove(name).is_some()
    }

    pub fn remove_session(&self, name: &str) -> Option<Arc<Session>> {
        let session = self.sessions.lock().unwrap().remove(name);
        self.remove_completed(name);
        session
    }

    pub fn require_removed_session(&self, name: &str) -> Result<Arc<Session>> {
        self.remove_session(name)
            .ok_or_else(|| anyhow!("pty {name} does not exist"))
    }
}
