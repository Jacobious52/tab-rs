// capture stdin and stdout
// watch for user input followed by an enter - this is probably a command
// capture the stdin + stdout for that line - that is a history entry
// save the last N history entries

use std::time::Duration;

pub struct Pattern {
    pub parts: Vec<Part>,
}

pub enum Part {
    /// The exact bytes, appearing in stdin
    Stdin(Vec<u8>),
    /// The exact bytes, appearing in stdout
    Stdout(Vec<u8>),
    /// Inactivity (both stdin and stdout) of at least this duration
    Inactivity(Duration),
}

pub struct PatternMatch {
    pub parts: Vec<Part>,
}

fn statusline_pattern() -> Pattern {
    let mut parts = Vec::new();
    parts.push(Part::Stdout("\n".as_bytes().into_iter().copied().collect()));
    parts.push(Part::Stdin("\n".as_bytes().into_iter().copied().collect()));

    Pattern { parts }
}
