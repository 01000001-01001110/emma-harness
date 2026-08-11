//! The wire: JSON-RPC 2.0 messages inside `Content-Length` frames, over stdio.
//!
//! This is the whole of LSP's transport and it is fifty lines, which is worth
//! saying because the alternative considered was a client crate. It was refused
//! for the usual reason a dependency gets refused here — the crate would bring a
//! generated model of every LSP type, of which this crate uses six, and the
//! parts that are actually hard (readiness, lifecycle, containment) are not the
//! parts a client crate solves.
//!
//! Three details in the framing are load-bearing and each has cost somebody a
//! day at some point:
//!
//! - **Header names are case-insensitive.** The spec says `Content-Length`;
//!   servers have shipped `content-length`. Matching the exact spelling works
//!   until it does not, and the failure is a hang rather than an error, because
//!   a frame whose length was not parsed is a frame nobody consumes.
//! - **The body is bytes, not characters.** `Content-Length` counts bytes, so
//!   the body is read with `read_exact` into a byte buffer. Reading it as a
//!   string and counting `char`s desynchronises the stream permanently on the
//!   first non-ASCII doc comment, and everything after that frame is garbage.
//! - **A short read at the header is end-of-stream, not an error.** The server
//!   exiting is an ordinary event this crate has to distinguish from a corrupt
//!   frame, because one means "it died" and the other means "it is confused";
//!   they are reported to the model as different sentences.
//!
//! There is a cap on the body length. A server that answers `Content-Length:
//! 4294967296` would otherwise have Emma allocate it before discovering the
//! frame is nonsense.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// region: Framing
// ---------------------------------------------------------------------------
// Framing
//
// One function each way, and a `Frame` that separates "the stream ended" from
// "the stream said something it should not have".
// ---------------------------------------------------------------------------

/// Refused rather than allocated. Real LSP messages are kilobytes; a
/// `documentSymbol` reply for a huge generated file is the largest thing seen
/// here and is nowhere near this.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// What one read produced.
///
/// `Eof` is a variant rather than an `Option` because the caller acts on it —
/// it is how a crashed server is noticed, and it is reported to the model
/// differently from a malformed frame.
#[derive(Debug)]
pub enum Frame {
    Message(Value),
    Eof,
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(message)?;
    // One `write_all` per part rather than one concatenated buffer: the header
    // is tiny and the body can be large, and concatenating would copy the body
    // for no reason. `flush` matters — the child is blocked on a pipe read.
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

/// Reads one frame: headers until a blank line, then exactly `Content-Length`
/// bytes.
///
/// Unknown headers are skipped rather than refused. `Content-Type` is the one
/// the spec defines and it carries nothing this crate needs; a server that adds
/// another should not break the transport.
pub async fn read_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> std::io::Result<Frame> {
    let mut length: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        // A zero-byte read is the child's stdout closing, which is how this
        // crate learns the server exited. Mid-headers it is still Eof: there is
        // no partial frame worth reporting, and the caller is about to reap the
        // process and get the real reason from its status and stderr.
        if reader.read_line(&mut line).await? == 0 {
            return Ok(Frame::Eof);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !saw_header {
                // A blank line before any header is a stream out of sync, not
                // an empty message. Reported rather than skipped, because
                // skipping would loop forever on a server printing newlines.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "a blank line arrived where a message header was expected",
                ));
            }
            break;
        }
        saw_header = true;
        let Some((name, value)) = line.split_once(':') else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("malformed message header: {line:?}"),
            ));
        };
        // Case-insensitively, because the spec's capitalisation is a
        // recommendation and a missed match here hangs rather than fails.
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = Some(value.trim().parse().map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Content-Length {:?} is not a number: {e}", value.trim()),
                )
            })?);
        }
    }

    let Some(length) = length else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a message arrived with no Content-Length header",
        ));
    };
    if length > MAX_MESSAGE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Content-Length {length} exceeds the {MAX_MESSAGE_BYTES}-byte ceiling"),
        ));
    }

    // Bytes, and `read_exact`. See the module doc: counting characters here
    // desynchronises the stream on the first non-ASCII byte and never recovers.
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    let value = serde_json::from_slice(&body).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the message body is not JSON: {e}"),
        )
    })?;
    Ok(Frame::Message(value))
}

// endregion: Framing

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The transport is the one part of this crate that needs no server at all, so
// it is tested exhaustively here and the harder things are tested against a
// fake in `tests/`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    async fn round_trip(messages: &[Value]) -> Vec<Value> {
        let mut buf: Vec<u8> = Vec::new();
        for m in messages {
            write_message(&mut buf, m).await.expect("write");
        }
        let mut reader = BufReader::new(std::io::Cursor::new(buf));
        let mut out = Vec::new();
        while let Frame::Message(v) = read_message(&mut reader).await.expect("read") {
            out.push(v);
        }
        out
    }

    #[tokio::test]
    async fn frames_survive_a_round_trip_back_to_back() {
        // Back to back is the case that matters: a reader that consumes one
        // byte too many or too few works perfectly for a single message and
        // corrupts everything after it.
        let sent = vec![
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "capabilities": {} } }),
            json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "t" } }),
        ];
        assert_eq!(round_trip(&sent).await, sent);
    }

    #[tokio::test]
    async fn the_length_is_bytes_and_not_characters() {
        // A doc comment with an em dash in it is the ordinary case, not an edge
        // case, and getting this wrong desynchronises every frame afterwards.
        // Two messages, so the failure is a corrupt *second* message rather
        // than merely a wrong first one.
        let sent = vec![
            json!({ "jsonrpc": "2.0", "id": 1, "result": "a — b — 日本語 — ✓" }),
            json!({ "jsonrpc": "2.0", "id": 2, "result": "after" }),
        ];
        let got = round_trip(&sent).await;
        assert_eq!(got, sent);

        // And the header really does count bytes, not chars.
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &json!("é")).await.expect("write");
        let text = String::from_utf8(buf).expect("ascii header");
        assert!(text.starts_with("Content-Length: 4\r\n\r\n"), "{text:?}");
    }

    #[tokio::test]
    async fn headers_are_matched_case_insensitively_and_extras_are_skipped() {
        let body = br#"{"jsonrpc":"2.0","id":7}"#;
        let raw = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        );
        let mut bytes = raw.into_bytes();
        bytes.extend_from_slice(body);
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        match read_message(&mut reader).await.expect("read") {
            Frame::Message(v) => assert_eq!(v["id"], 7),
            Frame::Eof => panic!("a complete frame read as end of stream"),
        }
    }

    #[tokio::test]
    async fn a_closed_stream_is_eof_and_not_an_error() {
        // This is how a crashed server is detected. If it came back as an error
        // the model would be told the transport is broken, when what happened
        // is that the process died — a different fact with a different fix.
        let mut reader = BufReader::new(std::io::Cursor::new(Vec::new()));
        assert!(matches!(
            read_message(&mut reader)
                .await
                .expect("eof is not an error"),
            Frame::Eof
        ));
    }

    #[tokio::test]
    async fn nonsense_on_the_wire_is_an_error_rather_than_a_hang() {
        // Each of these used to be a plausible way to wedge the reader forever.
        // A hang is the worst failure this crate can have: nothing is reported,
        // the turn stalls, and there is no message anywhere saying why.
        for raw in [
            "\r\n\r\n".to_string(),
            "Content-Length: banana\r\n\r\n".to_string(),
            "Content-Type: x\r\n\r\n{}".to_string(),
            format!("Content-Length: {}\r\n\r\n{{}}", MAX_MESSAGE_BYTES + 1),
            "not a header at all\r\n\r\n".to_string(),
        ] {
            let mut reader = BufReader::new(std::io::Cursor::new(raw.clone().into_bytes()));
            let err = read_message(&mut reader)
                .await
                .expect_err(&format!("{raw:?} must not be accepted"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{raw:?}");
        }
    }
}

// endregion: Tests
