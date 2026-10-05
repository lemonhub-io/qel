//! qel - a git implementation in pure Rust std.
mod commands;
mod config;
mod credential;
mod diff;
mod ignore;
mod index;
mod object;
mod odb;
mod pack;
mod pktline;
mod protocol;
mod refs;
mod repo;
mod revision;
mod revwalk;
mod sha1;
mod transport;
mod tree;
mod util;
mod worktree;
mod zlib;

use std::process::exit;

fn main() {
    // Rust ignores SIGPIPE so library writes surface as errors; a CLI
    // wants git's behavior — die quietly when the reader goes away
    // (`qel log | head -5` must not panic with "broken pipe").
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match commands::dispatch(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qel: {}", e);
            128
        }
    };
    exit(code);
}
