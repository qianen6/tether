use std::collections::VecDeque;

/// A byte-bounded ring. If a line is longer than the capacity, discard its
/// remainder across reads until LF, rather than retaining a UTF-8/ANSI suffix.
#[derive(Debug)]
pub(super) struct Scrollback {
    bytes: VecDeque<u8>,
    capacity: usize,
    discarding_line: bool,
}

impl Scrollback {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
            discarding_line: false,
        }
    }

    pub(super) fn push(&mut self, mut data: &[u8]) {
        if self.capacity == 0 {
            return;
        }
        if self.discarding_line {
            let Some(newline) = data.iter().position(|&b| b == b'\n') else {
                return;
            };
            data = &data[newline + 1..];
            self.discarding_line = false;
        }
        self.bytes.extend(data);
        if self.bytes.len() > self.capacity {
            let excess = self.bytes.len() - self.capacity;
            if let Some(next_lf) = self.bytes.iter().skip(excess - 1).position(|&b| b == b'\n') {
                self.bytes.drain(..excess + next_lf);
            } else {
                self.bytes.clear();
                self.discarding_line = true;
            }
        }
    }

    pub(super) fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }
}
