//! Setting the terminal's own font, from inside the terminal.
//!
//! **A stub, declared ahead of its port from the macOS fork.** The fork's
//! version uses `std::os::unix` unguarded and does not build on Windows; the
//! port gates on `TERM_PROGRAM` as data and reports plainly where the
//! terminal offers no such control.
