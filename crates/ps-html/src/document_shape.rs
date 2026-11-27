//! Conservative, bounded streaming diagnostic gate, not an HTML validator.
//! Require explicit source <html> and </html>; ignore comments/raw text.
#[derive(Default)]
pub struct DocumentShape {
    token: Vec<u8>,
    in_tag: bool,
    comment: bool,
    quote: u8,
    raw: Option<&'static [u8]>,
    opened: bool,
    invalid_prefix: bool,
    closed: bool,
    suffix: [u8; 3],
}
impl DocumentShape {
    pub fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.comment {
                self.suffix.rotate_left(1);
                self.suffix[2] = byte;
                if self.suffix == *b"-->" {
                    self.comment = false;
                }
                continue;
            }
            if !self.in_tag {
                if byte == b'<' {
                    self.in_tag = true;
                    self.token.clear();
                    self.quote = 0;
                } else if !self.opened && !byte.is_ascii_whitespace() {
                    self.invalid_prefix = true;
                }
                continue;
            }
            if self.token.len() < 256 {
                self.token.push(byte.to_ascii_lowercase());
            }
            if self.token == b"!--" {
                self.comment = true;
                self.in_tag = false;
                self.suffix = [0; 3];
                continue;
            }
            if self.quote != 0 {
                if byte == self.quote {
                    self.quote = 0;
                }
                continue;
            }
            if byte == b'\'' || byte == b'"' {
                self.quote = byte;
                continue;
            }
            if byte != b'>' {
                continue;
            }
            self.in_tag = false;
            let token = &self.token[..self.token.len().saturating_sub(1)];
            let closing = token.starts_with(b"/");
            let name = if closing { &token[1..] } else { token };
            let end = name
                .iter()
                .position(|b| b.is_ascii_whitespace() || *b == b'/')
                .unwrap_or(name.len());
            let name = &name[..end];
            if let Some(raw) = self.raw {
                if closing && name == raw {
                    self.raw = None;
                }
                continue;
            }
            if !self.opened && name != b"html" && name != b"!doctype" && !name.starts_with(b"?") {
                self.invalid_prefix = true;
            }
            if name == b"html" {
                if closing {
                    self.closed = self.opened;
                } else {
                    self.opened = true;
                }
            }
            if !closing {
                self.raw = match name {
                    b"script" => Some(b"script"),
                    b"style" => Some(b"style"),
                    b"textarea" => Some(b"textarea"),
                    b"title" => Some(b"title"),
                    b"xmp" => Some(b"xmp"),
                    b"plaintext" => Some(b"plaintext"),
                    _ => None,
                };
            }
        }
    }
    pub fn complete(&self) -> bool {
        self.opened && self.closed && !self.invalid_prefix
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_full_documents_only() {
        for (input, complete) in [
            ("1000", false),
            ("{\"markup\":\"<html></html>\"}", false),
            ("{\"price\":1000}", false),
            ("<div>1000</div>", false),
            ("<html><body>partial", false),
            ("<!-- <html></html> -->", false),
            ("<script>var x='<html></html>';</script>", false),
            ("<textarea><html></html></textarea>", false),
            (
                "<!doctype html><HTML lang='en'><body>1000</body></HTML>",
                true,
            ),
            ("<html><script>let x='</html>';</script></html>", true),
        ] {
            for width in [1, 2, 7, 1024] {
                let mut detector = DocumentShape::default();
                for chunk in input.as_bytes().chunks(width) {
                    detector.write(chunk);
                }
                assert_eq!(detector.complete(), complete, "{input}, chunks {width}");
            }
        }
    }
}
