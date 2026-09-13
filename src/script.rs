//! External module commands and the lines they print.
//!
//! One thread per module. A module either runs on a timer and reports the last
//! line it printed, or stays running and reports every line as it appears.
//!
//! Commands run through `sh -c`, so a config can say `date '+%H:%M'` rather than
//! having to name a program and separate its arguments.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::bar::ModuleValue;
use crate::config::{Config, Module, Rgba};

/// How long a module waits before starting a crashed stream again.
///
/// A command that fails instantly would otherwise be restarted in a tight loop.
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// How often a sleeping thread checks whether it should stop instead.
const STOP_POLL: Duration = Duration::from_millis(50);

/// Parse one line of module output.
///
/// A line that is a JSON object with a `text` field uses it, plus `color` when
/// that is a valid colour. Anything else is taken as the text itself, which is
/// what makes a config like `date '+%H:%M'` work without any wrapping. Blank
/// lines report nothing, so a module keeps whatever it was showing.
pub fn parse_line(line: &str) -> Option<ModuleValue> {
    if line.trim().is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
        && let Some(object) = value.as_object()
        && let Some(text) = object.get("text").and_then(|text| text.as_str())
    {
        // An unusable colour falls back to the default rather than throwing the
        // whole line away.
        let color = object
            .get("color")
            .and_then(|color| color.as_str())
            .and_then(|color| Rgba::parse(color).ok());
        return Some(ModuleValue {
            text: text.to_string(),
            color,
        });
    }

    Some(ModuleValue {
        text: line.to_string(),
        color: None,
    })
}

/// Every module's worker thread.
pub struct Modules {
    workers: Vec<Worker>,
}

impl Modules {
    /// Start one thread per configured module.
    pub fn start<F>(config: &Config, emit: F) -> Self
    where
        F: Fn(usize, ModuleValue) + Clone + Send + 'static,
    {
        let workers = config
            .bar
            .module
            .iter()
            .enumerate()
            .map(|(index, module)| Worker::start(index, module, emit.clone()))
            .collect();
        Self { workers }
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Stop every module process and wait for its thread to finish.
    ///
    /// Idempotent: dropping the modules does the same thing.
    pub fn stop(&mut self) {
        for worker in self.workers.drain(..) {
            worker.stop();
        }
    }
}

impl Drop for Modules {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    /// The running process, so that stopping can kill a blocked reader.
    child: Arc<Mutex<Option<Child>>>,
    handle: JoinHandle<()>,
}

impl Worker {
    fn start<F>(index: usize, module: &Module, emit: F) -> Self
    where
        F: Fn(usize, ModuleValue) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let name = module.name.clone();
        let exec = module.exec.clone();
        let stream = module.stream;
        let interval = Duration::from_secs(module.interval.unwrap_or(1).max(1));

        let handle = {
            let stop = Arc::clone(&stop);
            let child = Arc::clone(&child);
            thread::spawn(move || {
                if stream {
                    stream_loop(&name, &exec, &stop, &child, &emit, index);
                } else {
                    interval_loop(&name, &exec, interval, &stop, &emit, index);
                }
            })
        };

        Self { stop, child, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        // Killing the process unblocks the reader, which is otherwise parked in
        // a read that no flag can interrupt.
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = self.handle.join();
    }
}

/// Run the command, report its last line, wait out the interval, repeat.
fn interval_loop<F>(
    name: &str,
    exec: &str,
    interval: Duration,
    stop: &AtomicBool,
    emit: &F,
    index: usize,
) where
    F: Fn(usize, ModuleValue),
{
    while !stop.load(Ordering::Relaxed) {
        match run_once(exec) {
            // No output at all: keep whatever the module was showing.
            Ok(None) => {}
            Ok(Some(line)) => {
                if let Some(value) = parse_line(&line) {
                    emit(index, value);
                }
            }
            Err(error) => report(name, &error),
        }
        sleep(interval, stop);
    }
}

/// Run the command once and return its last non-blank line.
fn run_once(exec: &str) -> Result<Option<String>, String> {
    let mut child = spawn(exec)?;
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;

    let mut last = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|error| error.to_string())?;
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    child.wait().map_err(|error| error.to_string())?;
    Ok(last)
}

/// Keep the command running and report every line it prints.
fn stream_loop<F>(
    name: &str,
    exec: &str,
    stop: &AtomicBool,
    slot: &Mutex<Option<Child>>,
    emit: &F,
    index: usize,
) where
    F: Fn(usize, ModuleValue),
{
    while !stop.load(Ordering::Relaxed) {
        match spawn(exec) {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                *slot.lock().unwrap() = Some(child);
                if let Some(stdout) = stdout {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        if let Some(value) = parse_line(&line) {
                            emit(index, value);
                        }
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
                let mut child = slot.lock().unwrap().take();
                if let Some(child) = child.as_mut() {
                    let _ = child.wait();
                }
            }
            Err(error) => report(name, &error),
        }
        sleep(RESTART_DELAY, stop);
    }
}

fn spawn(exec: &str) -> Result<Child, String> {
    Command::new("sh")
        .arg("-c")
        .arg(exec)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())
}

/// Sleep, but notice a stop request within [`STOP_POLL`].
fn sleep(duration: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(STOP_POLL));
    }
}

fn report(name: &str, error: &str) {
    eprintln!("quickbar: module `{name}`: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};

    fn config_with(exec: &str, stream: bool) -> Config {
        let mut config = Config::default();
        config.bar.module = vec![Module {
            name: "test".to_string(),
            exec: exec.to_string(),
            interval: (!stream).then_some(1),
            stream,
            ..Module::default()
        }];
        config
    }

    /// Start the modules and hand back the receiver their updates arrive on.
    fn run(config: &Config) -> (Modules, Receiver<(usize, ModuleValue)>) {
        let (sender, receiver) = mpsc::channel();
        let modules = Modules::start(config, move |index, value| {
            let _ = sender.send((index, value));
        });
        (modules, receiver)
    }

    #[test]
    fn json_lines_carry_text_and_colour() {
        let value = parse_line(r##"{"text":"78%","color":"#a3be8c"}"##).unwrap();
        assert_eq!(value.text, "78%");
        assert_eq!(value.color, Some(Rgba::new(0xa3, 0xbe, 0x8c, 0xff)));
    }

    #[test]
    fn json_lines_without_a_colour_use_the_default() {
        let value = parse_line(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(value.text, "hi");
        assert_eq!(value.color, None);
    }

    #[test]
    fn an_unusable_colour_falls_back_instead_of_dropping_the_line() {
        let value = parse_line(r#"{"text":"hi","color":"red"}"#).unwrap();
        assert_eq!(value.text, "hi");
        assert_eq!(value.color, None);
    }

    #[test]
    fn plain_text_is_taken_as_is() {
        assert_eq!(parse_line("14:03").unwrap().text, "14:03");
        assert_eq!(parse_line("14:03").unwrap().color, None);
        assert_eq!(parse_line("  spaced  ").unwrap().text, "  spaced  ");
    }

    #[test]
    fn lines_that_only_look_like_json_stay_text() {
        assert_eq!(parse_line("{not json}").unwrap().text, "{not json}");
        assert_eq!(parse_line(r#"{"text":5}"#).unwrap().text, r#"{"text":5}"#);
        assert_eq!(parse_line(r#"{"other":"x"}"#).unwrap().text, r#"{"other":"x"}"#);
        assert_eq!(parse_line(r#"["a"]"#).unwrap().text, r#"["a"]"#);
    }

    #[test]
    fn blank_lines_report_nothing() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("\t").is_none());
    }

    #[test]
    fn an_interval_module_reports_its_last_line() {
        let config = config_with("printf 'one\\ntwo\\n'", false);
        let (mut modules, updates) = run(&config);

        let (index, value) = updates
            .recv_timeout(Duration::from_secs(10))
            .expect("the module should report within its interval");
        assert_eq!(index, 0);
        assert_eq!(value.text, "two", "the last non-blank line wins");

        modules.stop();
    }

    #[test]
    fn an_interval_module_with_no_output_reports_nothing() {
        let config = config_with("true", false);
        let (mut modules, updates) = run(&config);

        assert!(
            updates.recv_timeout(Duration::from_millis(1500)).is_err(),
            "silence must not become an empty module value"
        );

        modules.stop();
    }

    #[test]
    fn a_stream_module_reports_every_line() {
        let config = config_with("printf 'first\\n'; sleep 30", true);
        let (mut modules, updates) = run(&config);

        let (_, first) = updates.recv_timeout(Duration::from_secs(10)).expect("first line");
        assert_eq!(first.text, "first");

        modules.stop();
    }

    #[test]
    fn a_stream_that_keeps_printing_is_read_line_by_line() {
        let config = config_with("printf 'a\\nb\\nc\\n'; sleep 30", true);
        let (mut modules, updates) = run(&config);

        let mut seen = Vec::new();
        while seen.len() < 3 {
            let (_, value) = updates.recv_timeout(Duration::from_secs(10)).expect("a line");
            seen.push(value.text);
        }
        assert_eq!(seen, ["a", "b", "c"]);

        modules.stop();
    }

    #[test]
    fn a_stream_that_exits_is_restarted_after_the_delay() {
        // Exits immediately, so the only way to see two reports is a restart.
        let config = config_with("printf 'tick\\n'", true);
        let (mut modules, updates) = run(&config);

        updates.recv_timeout(Duration::from_secs(10)).expect("first run");
        let started = Instant::now();
        updates.recv_timeout(Duration::from_secs(10)).expect("restart");
        assert!(
            started.elapsed() >= RESTART_DELAY,
            "restarts must not spin: restarted after {:?}",
            started.elapsed()
        );

        modules.stop();
    }

    #[test]
    fn a_missing_command_is_reported_and_survived() {
        let config = config_with("/nonexistent/quickbar-test-command", false);
        let (mut modules, updates) = run(&config);

        assert!(
            updates.recv_timeout(Duration::from_millis(1500)).is_err(),
            "a failing module reports nothing"
        );
        // Still stoppable, and the failure did not take the thread with it.
        modules.stop();
    }

    #[test]
    fn stopping_joins_a_stream_that_is_still_running() {
        let config = config_with("sleep 300", true);
        let (mut modules, _updates) = run(&config);
        thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        modules.stop();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stop should kill the process rather than wait for it: took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_module_reports_its_configured_index() {
        let mut config = Config::default();
        config.bar.module = vec![
            Module {
                name: "first".to_string(),
                exec: "printf 'a\\n'".to_string(),
                interval: Some(1),
                ..Module::default()
            },
            Module {
                name: "second".to_string(),
                exec: "printf 'b\\n'".to_string(),
                interval: Some(1),
                ..Module::default()
            },
        ];
        let (mut modules, updates) = run(&config);

        let mut seen = Vec::new();
        while seen.len() < 2 {
            seen.push(updates.recv_timeout(Duration::from_secs(10)).expect("an update"));
        }
        seen.sort_by_key(|(index, _)| *index);
        assert_eq!(seen[0], (0, ModuleValue { text: "a".to_string(), color: None }));
        assert_eq!(seen[1], (1, ModuleValue { text: "b".to_string(), color: None }));

        modules.stop();
    }

    #[test]
    fn no_modules_means_no_threads() {
        let config = Config::default();
        let (mut modules, _updates) = run(&config);

        assert!(modules.is_empty());
        modules.stop();
    }
}
