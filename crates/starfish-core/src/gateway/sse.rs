//! Emulated-streaming plumbing: a channel of SSE events rendered as an
//! `text/event-stream` response body.
//!
//! Hyperagent doesn't push tokens, so surface handlers spawn a task that runs
//! the poll loop and feeds events into this channel; the response streams
//! whatever arrives (MISSION.md §4, "Streaming is emulated").

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

/// One server-sent event. `event: None` emits a bare `data:` line (OpenAI
/// style); `Some(name)` emits `event: name` first (Anthropic/Responses style).
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseEvent {
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
        }
    }

    pub fn named(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(name) = &self.event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        // Multi-line data must become multiple data: lines.
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

pub type SseSender = mpsc::UnboundedSender<SseEvent>;

/// Build the streaming response from a fresh channel; the caller spawns the
/// producer task with the sender.
pub fn channel_response() -> (SseSender, Response) {
    let (tx, rx) = mpsc::unbounded_channel::<SseEvent>();
    let stream = UnboundedReceiverStream::new(rx)
        .map(|ev| Ok::<Bytes, std::convert::Infallible>(Bytes::from(ev.render())));
    let body = Body::from_stream(stream);
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(body)
        .expect("static response parts");
    (tx, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_data_event() {
        let ev = SseEvent::data("{\"a\":1}");
        assert_eq!(ev.render(), "data: {\"a\":1}\n\n");
    }

    #[test]
    fn named_event() {
        let ev = SseEvent::named("message_start", "{}");
        assert_eq!(ev.render(), "event: message_start\ndata: {}\n\n");
    }

    #[test]
    fn multiline_data_gets_split() {
        let ev = SseEvent::data("line1\nline2");
        assert_eq!(ev.render(), "data: line1\ndata: line2\n\n");
    }
}
