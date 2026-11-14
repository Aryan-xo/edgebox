//! Recent stdout/stderr lines of each function, also emitted as tracing events.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use edgebox_runtime::{LogSink, LogStream};
use serde::Serialize;

/// Lines kept per function; older lines are dropped.
const MAX_LINES: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Unix timestamp, milliseconds.
    pub timestamp_ms: u64,
    pub stream: &'static str,
    pub message: String,
}

#[derive(Default)]
pub struct FunctionLogs {
    lines: Mutex<HashMap<String, VecDeque<LogLine>>>,
}

impl FunctionLogs {
    /// A sink for the output of `function`.
    pub fn sink(self: &Arc<Self>, function: &str) -> LogSink {
        let logs = self.clone();
        let function = function.to_string();
        Arc::new(move |stream: LogStream, message: &str| {
            tracing::info!(
                target: "edgebox::function",
                function = %function,
                stream = stream.as_str(),
                "{message}"
            );
            logs.push(&function, stream, message);
        })
    }

    fn push(&self, function: &str, stream: LogStream, message: &str) {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut lines = self.lines.lock().unwrap();
        let lines = lines.entry(function.to_string()).or_default();
        if lines.len() == MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(LogLine {
            timestamp_ms,
            stream: stream.as_str(),
            message: message.to_string(),
        });
    }

    /// The most recent lines of `function`, oldest first.
    pub fn recent(&self, function: &str) -> Vec<LogLine> {
        let lines = self.lines.lock().unwrap();
        lines
            .get(function)
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn remove(&self, function: &str) {
        self.lines.lock().unwrap().remove(function);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_lines_per_function() {
        let logs = Arc::new(FunctionLogs::default());
        let sink = logs.sink("a");
        for i in 0..MAX_LINES + 5 {
            sink(LogStream::Stdout, &format!("line {i}"));
        }
        logs.sink("b")(LogStream::Stderr, "other");

        let recent = logs.recent("a");
        assert_eq!(recent.len(), MAX_LINES);
        assert_eq!(recent[0].message, "line 5");
        assert_eq!(
            recent.last().unwrap().message,
            format!("line {}", MAX_LINES + 4)
        );
        assert_eq!(logs.recent("b")[0].stream, "stderr");

        logs.remove("a");
        assert!(logs.recent("a").is_empty());
    }
}
