use serde::{Deserialize, Serialize};

fn to_string(data: &[u8]) -> String {
    snailquote::escape(std::str::from_utf8(data).unwrap_or("")).to_string()
}

/// Serializes an indexed chunk of stdout
/// Send by a single running PTY process.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
    /// The message index, generated by the PTY process.
    pub index: usize,
    /// Raw bytes of stdout
    pub data: Vec<u8>,
}

impl OutputChunk {
    /// The data buffer length
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns true if this chunk's data contains the given index
    pub fn contains(&self, index: usize) -> bool {
        index >= self.start() && index < self.end()
    }

    /// Returns true if this chunk's data ends before the given index
    pub fn is_before(&self, index: usize) -> bool {
        self.end() <= index
    }

    /// Truncates the current output chunk, removing all data that is before the given index
    pub fn truncate_before(&mut self, index: usize) {
        if index <= self.start() {
            return;
        }

        if index >= self.end() {
            self.data.clear();
            return;
        }

        let data_index = index - self.start();
        self.data.drain(0..data_index);
        self.index = index;
    }

    /// The byte index at which this buffer starts (inclusive)
    pub fn start(&self) -> usize {
        self.index
    }

    /// The byte index at which this buffer ends (exclusive)
    pub fn end(&self) -> usize {
        self.index + self.data.len()
    }
}

impl ToString for OutputChunk {
    fn to_string(&self) -> String {
        to_string(self.data.as_slice())
    }
}

/// Serialize an unindexed chunk of stdin.
/// May be sent by multiple CLI connections.
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct InputChunk {
    /// Raw bytes of stdin
    pub data: Vec<u8>,
}

impl InputChunk {
    /// THe data buffer length
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

impl ToString for InputChunk {
    fn to_string(&self) -> String {
        to_string(self.data.as_slice())
    }
}
