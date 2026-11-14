//! Routes a function's stdout/stderr to a callback, one line at a time.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};

/// Lines longer than this are split.
const MAX_LINE_BYTES: usize = 16 * 1024;

/// Which output stream a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Receives each line a function prints. Called inline, so keep it cheap.
pub type LogSink = Arc<dyn Fn(LogStream, &str) + Send + Sync>;

/// A WASI stdout/stderr that forwards complete lines to a [`LogSink`].
#[derive(Clone)]
pub(crate) struct LogPipe {
    pub(crate) sink: LogSink,
    pub(crate) stream: LogStream,
}

impl IsTerminal for LogPipe {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for LogPipe {
    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(LineWriter {
            pipe: self.clone(),
            buf: Vec::new(),
        })
    }
}

struct LineWriter {
    pipe: LogPipe,
    buf: Vec<u8>,
}

impl LineWriter {
    fn emit(&self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        (self.pipe.sink)(self.pipe.stream, line.trim_end_matches('\r'));
    }

    fn emit_complete_lines(&mut self) {
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            self.emit(&line[..end]);
        }
        while self.buf.len() >= MAX_LINE_BYTES {
            let chunk: Vec<u8> = self.buf.drain(..MAX_LINE_BYTES).collect();
            self.emit(&chunk);
        }
    }

    fn emit_rest(&mut self) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.emit(&rest);
        }
    }
}

impl AsyncWrite for LineWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.buf.extend_from_slice(data);
        this.emit_complete_lines();
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().emit_rest();
        Poll::Ready(Ok(()))
    }
}

impl Drop for LineWriter {
    /// Output without a trailing newline is still logged.
    fn drop(&mut self) {
        self.emit_rest();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn splits_output_into_lines() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = lines.clone();
        let pipe = LogPipe {
            sink: Arc::new(move |stream, line| {
                captured.lock().unwrap().push((stream, line.to_string()));
            }),
            stream: LogStream::Stderr,
        };

        let mut writer = Pin::from(pipe.async_stream());
        writer.write_all(b"one\r\ntw").await.unwrap();
        writer.write_all(b"o\nthree").await.unwrap();
        drop(writer);

        let lines = lines.lock().unwrap();
        let text: Vec<_> = lines.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(text, ["one", "two", "three"]);
        assert!(lines.iter().all(|(s, _)| *s == LogStream::Stderr));
    }
}
