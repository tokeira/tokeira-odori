use std::{io, process::ExitStatus};

use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, Command},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const CHANNEL_CAPACITY: usize = 64;
const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HarnessError {
    #[error("failed to decode harness stdout as JSON: {source}")]
    Decode {
        line: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to encode harness stdin as JSON")]
    Encode(#[source] serde_json::Error),
    #[error("harness stream I/O failed")]
    Io(#[from] io::Error),
    #[error("harness I/O task failed")]
    Task(#[from] tokio::task::JoinError),
    #[error("harness stdout reached EOF")]
    Eof,
    #[error("harness stdin is closed")]
    StdinClosed,
}

pub(crate) struct HarnessLauncher;

impl HarnessLauncher {
    pub(crate) fn launch<Event, Message>(
        mut command: Command,
        cancellation: CancellationToken,
    ) -> io::Result<HarnessHandle<Event, Message>>
    where
        Event: DeserializeOwned + Send + 'static,
        Message: Serialize + Send + 'static,
    {
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin_pipe = child
            .stdin
            .take()
            .expect("launcher always pipes harness stdin");
        let stdout = child
            .stdout
            .take()
            .expect("launcher always pipes harness stdout");
        let stderr = child
            .stderr
            .take()
            .expect("launcher always pipes harness stderr");
        let (event_sender, events) = mpsc::channel(CHANNEL_CAPACITY);
        let (stdin, messages) = mpsc::channel(CHANNEL_CAPACITY);
        drop(tokio::spawn(read_stdout(
            stdout,
            event_sender.clone(),
            cancellation.clone(),
        )));
        let stderr_task = tokio::spawn(capture_stderr(stderr));
        drop(tokio::spawn(write_stdin(
            stdin_pipe,
            messages,
            event_sender,
            cancellation.clone(),
        )));
        Ok(HarnessHandle {
            child,
            events,
            stderr_task,
            stdin: Some(stdin),
            cancellation,
        })
    }
}

pub(crate) struct HarnessHandle<Event, Message> {
    child: Child,
    events: mpsc::Receiver<Result<Event, HarnessError>>,
    stderr_task: JoinHandle<io::Result<String>>,
    stdin: Option<mpsc::Sender<Message>>,
    cancellation: CancellationToken,
}

impl<Event, Message> HarnessHandle<Event, Message> {
    pub(crate) async fn next_event(&mut self) -> Option<Result<Event, HarnessError>> {
        self.events.recv().await
    }

    pub(crate) async fn send(&self, message: Message) -> Result<(), HarnessError> {
        self.stdin
            .as_ref()
            .ok_or(HarnessError::StdinClosed)?
            .send(message)
            .await
            .map_err(|_| HarnessError::StdinClosed)
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        tokio::select! {
            status = self.child.wait() => status,
            () = self.cancellation.cancelled() => match self.child.start_kill() {
                Ok(()) => self.child.wait().await,
                Err(kill_error) => match self.child.try_wait()? {
                    Some(status) => Ok(status),
                    None => Err(kill_error),
                },
            },
        }
    }

    pub(crate) async fn collect_stderr(self) -> Result<String, HarnessError> {
        let Self { stderr_task, .. } = self;
        Ok(stderr_task.await??)
    }
}

async fn write_stdin<Event, Message>(
    mut stdin: tokio::process::ChildStdin,
    mut messages: mpsc::Receiver<Message>,
    event_sender: mpsc::Sender<Result<Event, HarnessError>>,
    cancellation: CancellationToken,
) where
    Event: Send + 'static,
    Message: Serialize + Send + 'static,
{
    loop {
        let message = tokio::select! {
            () = cancellation.cancelled() => break,
            message = messages.recv() => match message {
                Some(message) => message,
                None => break,
            },
        };
        let mut bytes = match serde_json::to_vec(&message) {
            Ok(bytes) => bytes,
            Err(source) => {
                if !send_event(
                    &event_sender,
                    &cancellation,
                    Err(HarnessError::Encode(source)),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };
        bytes.push(b'\n');
        let write = tokio::select! {
            () = cancellation.cancelled() => break,
            result = stdin.write_all(&bytes) => result,
        };
        if let Err(source) = write {
            send_event(&event_sender, &cancellation, Err(HarnessError::Io(source))).await;
            break;
        }
        let flush = tokio::select! {
            () = cancellation.cancelled() => break,
            result = stdin.flush() => result,
        };
        if let Err(source) = flush {
            send_event(&event_sender, &cancellation, Err(HarnessError::Io(source))).await;
            break;
        }
    }
}

async fn read_stdout<Event>(
    stdout: tokio::process::ChildStdout,
    sender: mpsc::Sender<Result<Event, HarnessError>>,
    cancellation: CancellationToken,
) where
    Event: DeserializeOwned + Send + 'static,
{
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = tokio::select! {
            () = cancellation.cancelled() => break,
            result = lines.next_line() => result,
        };
        let event = match line {
            Ok(Some(line)) => {
                serde_json::from_str(&line).map_err(|source| HarnessError::Decode { line, source })
            }
            Ok(None) => Err(HarnessError::Eof),
            Err(source) => Err(HarnessError::Io(source)),
        };
        let terminal = matches!(event, Err(HarnessError::Eof | HarnessError::Io(_)));
        if !send_event(&sender, &cancellation, event).await || terminal {
            break;
        }
    }
}

async fn send_event<Event>(
    sender: &mpsc::Sender<Result<Event, HarnessError>>,
    cancellation: &CancellationToken,
    event: Result<Event, HarnessError>,
) -> bool {
    tokio::select! {
        () = cancellation.cancelled() => false,
        result = sender.send(event) => result.is_ok(),
    }
}

async fn capture_stderr(mut stderr: ChildStderr) -> io::Result<String> {
    let mut captured = Vec::with_capacity(STDERR_LIMIT);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stderr.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = STDERR_LIMIT.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&captured).into_owned())
}
