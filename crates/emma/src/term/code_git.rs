//! Git backing for the Code page: the repository's files, one file's commit
//! history, and the diff a commit introduced to it.
//!
//! **A stub, declared ahead of its port from the macOS fork.** The fork's
//! save path re-joined lines with `\n` and so rewrote every CRLF file as LF;
//! the port keeps the file's own line ending and proves it with a
//! byte-identical round trip.
