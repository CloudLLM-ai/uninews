//! Integration tests for the scrape event system (`uninews::events`).
//!
//! The listener is process-wide state, so every test in this binary
//! serializes on `LISTENER_LOCK` and always restores the "no listener"
//! state before releasing the lock.

use std::sync::{Arc, Mutex};

use uninews::events::emit_event;
use uninews::{set_event_listener, ScrapeEvent};

/// Guards the process-wide listener slot against parallel tests in this
/// binary racing each other.
static LISTENER_LOCK: Mutex<()> = Mutex::new(());

/// A listener that records every event it receives into a shared buffer.
fn recording_listener() -> (Arc<Mutex<Vec<ScrapeEvent>>>, uninews::ScrapeEventListener) {
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let listener: uninews::ScrapeEventListener = Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    });
    (events, listener)
}

#[test]
fn listener_receives_emitted_events() {
    let _lock = LISTENER_LOCK.lock().unwrap();
    let (events, listener) = recording_listener();
    set_event_listener(Some(listener));

    emit_event(ScrapeEvent::ScrapeStarted {
        url: "https://example.com/a".to_string(),
    });
    emit_event(ScrapeEvent::ScrapeCompleted {
        url: "https://example.com/a".to_string(),
    });

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(matches!(recorded[0], ScrapeEvent::ScrapeStarted { .. }));
    assert!(matches!(recorded[1], ScrapeEvent::ScrapeCompleted { .. }));

    set_event_listener(None);
}

#[test]
fn clearing_the_listener_stops_delivery() {
    let _lock = LISTENER_LOCK.lock().unwrap();
    let (events, listener) = recording_listener();
    set_event_listener(Some(listener));
    set_event_listener(None);

    emit_event(ScrapeEvent::ScrapeStarted {
        url: "https://example.com/b".to_string(),
    });

    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn set_event_listener_returns_the_previous_listener() {
    let _lock = LISTENER_LOCK.lock().unwrap();
    let (_events_a, listener_a) = recording_listener();
    let (_events_b, listener_b) = recording_listener();

    assert!(set_event_listener(Some(listener_a)).is_none());
    let previous = set_event_listener(Some(listener_b));
    assert!(previous.is_some(), "expected listener A back");

    set_event_listener(None);
}

#[test]
fn panicking_listener_does_not_abort_the_emitter() {
    let _lock = LISTENER_LOCK.lock().unwrap();
    let panicking: uninews::ScrapeEventListener = Arc::new(|_event: &ScrapeEvent| {
        panic!("listener bug");
    });
    set_event_listener(Some(panicking));

    // Must not panic: listener failures are caught and reported to stderr.
    emit_event(ScrapeEvent::FetchFailed {
        url: "https://example.com/c".to_string(),
        error: "boom".to_string(),
    });

    set_event_listener(None);
}

#[test]
fn events_serialize_with_a_snake_case_event_tag() {
    let event = ScrapeEvent::FetchSucceeded {
        url: "https://example.com/d".to_string(),
        status: 200,
        body_bytes: 1234,
    };
    let json = serde_json::to_value(&event).expect("event must serialize");

    assert_eq!(json["event"], "fetch_succeeded");
    assert_eq!(json["url"], "https://example.com/d");
    assert_eq!(json["status"], 200);
    assert_eq!(json["body_bytes"], 1234);
}

#[test]
fn archive_fallback_events_serialize() {
    let json = serde_json::to_value(ScrapeEvent::ArchiveSnapshotFound {
        url: "https://example.com/e".to_string(),
        snapshot_url: "https://web.archive.org/web/20240101000000/https://example.com/e"
            .to_string(),
        timestamp: "20240101000000".to_string(),
    })
    .expect("event must serialize");

    assert_eq!(json["event"], "archive_snapshot_found");
    assert_eq!(json["timestamp"], "20240101000000");
}
