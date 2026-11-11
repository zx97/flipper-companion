//! Application log: a `tracing` subscriber that feeds the Log tab and a file.
//!
//! Every event that passes the active filter is mirrored three ways:
//!
//! - into an in-memory channel consumed by the GUI (the Log tab),
//! - into `flipper-companion.0.log`, next to the other rotated files,
//! - onto stderr, which is useful when the application is started from a
//!   terminal.
//!
//! The GUI selects the verbosity (errors, normal, verbose) and the filter is
//! reloaded in place, so debug logging is only formatted when it is shown.

use std::{
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter, Layer, Registry, filter::LevelFilter, fmt::format::FmtSpan, layer::SubscriberExt,
    reload, util::SubscriberInitExt,
};

/// Number of rotated log files kept on disk.
const ROTATED_FILES: usize = 10;

/// Severity of one record, ordered from most to least important.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Highest level shown for a GUI verbosity index (0 = errors, 2 = verbose).
    #[must_use]
    pub fn from_verbosity(verbosity: u8) -> Self {
        match verbosity {
            0 => Self::Error,
            1 => Self::Info,
            _ => Self::Trace,
        }
    }

    fn from_tracing(level: &Level) -> Self {
        match *level {
            Level::ERROR => Self::Error,
            Level::WARN => Self::Warn,
            Level::INFO => Self::Info,
            Level::DEBUG => Self::Debug,
            Level::TRACE => Self::Trace,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

/// One line of the application log.
#[derive(Debug, Clone)]
pub struct LogRecord {
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub elapsed: Duration,
}

impl LogRecord {
    /// Formats the record for the Log tab and the log file.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{:>9.3} {:>5} {}: {}",
            self.elapsed.as_secs_f64(),
            self.level.label(),
            self.target,
            self.message
        )
    }
}

/// Installs the subscriber and returns the GUI receiver plus the log path.
///
/// The log file is optional: a read-only home directory only disables file
/// logging, the Log tab keeps working.
pub fn init() -> (UnboundedReceiver<LogRecord>, Option<PathBuf>) {
    let (sender, receiver) = unbounded_channel();

    let file = FileSink::open();
    let path = file.as_ref().map(|sink| sink.path.clone());

    let _ = START.set(Instant::now());

    let (filter, handle) = reload::Layer::new(filter_from_env());
    let _ = FILTER.set(handle);

    let sink = ChannelSink {
        sender,
        file: file.map(Mutex::new),
        start: Instant::now(),
    };

    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .without_time()
        .compact()
        .with_span_events(FmtSpan::NONE);

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(stderr.with_filter(LevelFilter::INFO))
        .with(sink);

    if let Err(error) = registry.try_init() {
        eprintln!("log subscriber already installed: {error}");
    }

    (receiver, path)
}

/// Changes the active verbosity.
///
/// `verbosity` follows the GUI selector: 0 = errors only, 1 = normal,
/// 2 = verbose (debug and trace).
pub fn set_verbosity(verbosity: u8) {
    let directive = match verbosity {
        0 => "error",
        1 => "info",
        _ => "debug",
    };

    if let Some(handle) = FILTER.get() {
        let _ = handle.reload(EnvFilter::new(directive));
    }
}

static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Time since the application started, used for records built by the GUI.
#[must_use]
pub fn elapsed() -> Duration {
    START.get().map_or(Duration::ZERO, Instant::elapsed)
}

fn filter_from_env() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `tracing` layer that forwards events to the GUI and to the log file.
struct ChannelSink {
    sender: UnboundedSender<LogRecord>,
    file: Option<Mutex<FileSink>>,
    start: Instant,
}

impl<S: Subscriber> Layer<S> for ChannelSink {
    fn on_event(&self, event: &Event<'_>, _context: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let record = LogRecord {
            level: LogLevel::from_tracing(event.metadata().level()),
            target: event.metadata().target().to_owned(),
            message: visitor.finish(),
            elapsed: self.start.elapsed(),
        };

        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
            && let Err(error) = file.write(&record)
        {
            eprintln!("cannot write the log file: {error}");
        }

        // The Log tab is about this application: dependency chatter (winit,
        // reqwest, …) is only kept when it reaches a warning.
        if record.target.starts_with("flipper") || record.level <= LogLevel::Warn {
            let _ = self.sender.send(record);
        }
    }
}

/// Collects the `message` field and any structured fields of an event.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            return self.message;
        }

        let mut message = self.message;
        message.push_str(" (");
        message.push_str(
            &self
                .fields
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        message.push(')');
        message
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.fields
                .push((field.name().to_owned(), value.to_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = fmt::Write::write_fmt(&mut self.message, format_args!("{value:?}"));
        } else {
            self.fields
                .push((field.name().to_owned(), format!("{value:?}")));
        }
    }
}

/// Rotating log file, one file per run and up to [`ROTATED_FILES`] runs kept.
struct FileSink {
    path: PathBuf,
    file: fs::File,
}

impl FileSink {
    fn open() -> Option<Self> {
        let directory = data_dir()?.join("flipper-companion").join("logs");

        if fs::create_dir_all(&directory).is_err() {
            return None;
        }

        rotate(&directory);

        let path = directory.join("flipper-companion.log");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .ok()?;

        Some(Self { path, file })
    }

    fn write(&mut self, record: &LogRecord) -> std::io::Result<()> {
        writeln!(self.file, "{}", record.line())?;
        self.file.flush()
    }
}

fn data_dir() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

/// Shifts the previous runs to `.1` … `.9` so a fresh run starts unnumbered.
fn rotate(directory: &Path) {
    let _ = fs::remove_file(directory.join(format!("flipper-companion.{}.log", ROTATED_FILES - 1)));

    for index in (1..ROTATED_FILES - 1).rev() {
        let from = directory.join(format!("flipper-companion.{index}.log"));
        let to = directory.join(format!("flipper-companion.{}.log", index + 1));

        if from.exists() {
            let _ = fs::rename(&from, to);
        }
    }

    let current = directory.join("flipper-companion.log");

    if current.exists() {
        let _ = fs::rename(current, directory.join("flipper-companion.1.log"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_verbosity_to_levels() {
        assert_eq!(LogLevel::from_verbosity(0), LogLevel::Error);
        assert_eq!(LogLevel::from_verbosity(1), LogLevel::Info);
        assert_eq!(LogLevel::from_verbosity(2), LogLevel::Trace);
    }

    #[test]
    fn formats_records_with_a_level_and_target() {
        let record = LogRecord {
            level: LogLevel::Info,
            target: String::from("flipper_companion_app"),
            message: String::from("scan: 1 device"),
            elapsed: Duration::from_millis(1500),
        };

        let line = record.line();

        assert!(line.contains("1.500"), "{line}");
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains("flipper_companion_app: scan: 1 device"), "{line}");
    }

    #[test]
    fn collects_message_and_fields() {
        let mut visitor = MessageVisitor::default();

        {
            // `record_str`/`record_debug` need a `Field`, which is only
            // available from inside a tracing event; exercise `finish`.
            visitor.message.push_str("hello");
            visitor
                .fields
                .push((String::from("path"), String::from("/ext")));
        }

        assert_eq!(visitor.finish(), "hello (path=/ext)");
    }
}
