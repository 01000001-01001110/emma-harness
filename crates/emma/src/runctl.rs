//! Acting on a run, rather than only reading about one.
//!
//! `harness_state` answers "what ran"; this module is the other half: pause,
//! resume, cancel, archive and delete, plus launching a new run.
//!
//! **A stub, declared ahead of its port from the macOS fork** so that the
//! package that fills it owns this one file and nothing shared. Windows has
//! no supported per-process suspend; the port states that in words on the
//! Harness page rather than drawing a control that lies.
