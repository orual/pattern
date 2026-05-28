use rustyline_async::SharedWriter;
use std::io::{self, Write};
use tracing_subscriber::fmt::MakeWriter;

/// A writer that coordinates with rustyline-async's SharedWriter
/// to ensure logs don't interfere with the readline prompt
#[derive(Clone)]
pub struct TracingWriter {
    writer: Option<SharedWriter>,
    fallback: WriterTarget,
}

#[derive(Clone)]
enum WriterTarget {
    #[allow(dead_code)]
    Stdout,
    Stderr,
}

impl TracingWriter {
    /// Create a new tracing writer that will use SharedWriter when available
    pub fn new_stderr() -> Self {
        Self {
            writer: None,
            fallback: WriterTarget::Stderr,
        }
    }

    /// Set the SharedWriter to use for coordinated output
    pub fn set_shared_writer(&mut self, writer: SharedWriter) {
        self.writer = Some(writer);
    }

    /// Create a clone with the SharedWriter set
    #[allow(dead_code)]
    pub fn with_shared_writer(&self, writer: SharedWriter) -> Self {
        let mut new = self.clone();
        new.writer = Some(writer);
        new
    }
}

impl Write for TracingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(ref mut writer) = self.writer {
            // Use the SharedWriter which properly handles terminal control
            writer.write(buf)
        } else {
            // Fallback to stderr when SharedWriter isn't available (before chat starts)
            match self.fallback {
                WriterTarget::Stdout => io::stdout().write(buf),
                WriterTarget::Stderr => io::stderr().write(buf),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut writer) = self.writer {
            writer.flush()
        } else {
            match self.fallback {
                WriterTarget::Stdout => io::stdout().flush(),
                WriterTarget::Stderr => io::stderr().flush(),
            }
        }
    }
}

/// Global tracing writer instance
static TRACING_WRITER: std::sync::Mutex<Option<TracingWriter>> = std::sync::Mutex::new(None);

/// Initialize the global tracing writer
pub fn init_tracing_writer() -> TracingWriter {
    let writer = TracingWriter::new_stderr();
    *TRACING_WRITER.lock().unwrap() = Some(writer.clone());
    writer
}

/// Update the global tracing writer with a SharedWriter
pub fn set_shared_writer(shared_writer: SharedWriter) {
    if let Ok(mut guard) = TRACING_WRITER.lock() {
        if let Some(ref mut writer) = *guard {
            writer.set_shared_writer(shared_writer);
        }
    }
}

/// Get a clone of the global tracing writer
#[allow(dead_code)]
pub fn get_tracing_writer() -> Option<TracingWriter> {
    TRACING_WRITER.lock().ok()?.clone()
}

/// Get the currently configured SharedWriter (if any)
pub fn get_shared_writer() -> Option<SharedWriter> {
    let guard = TRACING_WRITER.lock().ok()?;
    let tw = guard.as_ref()?;
    tw.writer.clone()
}

/// Implement MakeWriter for TracingWriter
impl<'a> MakeWriter<'a> for TracingWriter {
    type Writer = TracingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        get_tracing_writer().unwrap_or(self.clone())
    }
}
