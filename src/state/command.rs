#[derive(Debug, Default)]
pub struct CommandBuffer {
    pub input: String,
    pub cursor: usize,
}

impl CommandBuffer {
    pub fn insert(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev_len = char_len_before(&self.input, self.cursor);
        self.cursor -= prev_len;
        self.input.remove(self.cursor);
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= char_len_before(&self.input, self.cursor);
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        self.cursor += char_len_after(&self.input, self.cursor);
    }
}

fn char_len_before(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx]
        .chars()
        .next_back()
        .map(char::len_utf8)
        .unwrap_or(0)
}

fn char_len_after(s: &str, byte_idx: usize) -> usize {
    s[byte_idx..]
        .chars()
        .next()
        .map(char::len_utf8)
        .unwrap_or(0)
}
