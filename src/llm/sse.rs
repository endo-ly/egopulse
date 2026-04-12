pub(crate) use parser::SseEventParser;

mod parser {
    #[derive(Default)]
    pub(crate) struct SseEventParser {
        pending: Vec<u8>,
        data_lines: Vec<String>,
    }

    impl SseEventParser {
        pub(crate) fn push_chunk(&mut self, chunk: impl AsRef<[u8]>) -> Vec<String> {
            self.pending.extend_from_slice(chunk.as_ref());
            let mut events = Vec::new();
            while let Some(pos) = self.pending.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = Self::decode_line(line);
                if let Some(event_data) = self.handle_line(&line) {
                    events.push(event_data);
                }
            }
            events
        }

        pub(crate) fn finish(&mut self) -> Vec<String> {
            let mut events = Vec::new();
            if !self.pending.is_empty() {
                let mut line = std::mem::take(&mut self.pending);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = Self::decode_line(line);
                if let Some(event_data) = self.handle_line(&line) {
                    events.push(event_data);
                }
            }
            if let Some(event_data) = self.flush_event() {
                events.push(event_data);
            }
            events
        }

        fn decode_line(line: Vec<u8>) -> String {
            match String::from_utf8(line) {
                Ok(line) => line,
                Err(error) => String::from_utf8_lossy(&error.into_bytes()).into_owned(),
            }
        }

        fn handle_line(&mut self, line: &str) -> Option<String> {
            if line.is_empty() {
                return self.flush_event();
            }
            if line.starts_with(':') {
                return None;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            if field == "data" {
                self.data_lines.push(value.to_string());
            }
            None
        }

        fn flush_event(&mut self) -> Option<String> {
            if self.data_lines.is_empty() {
                return None;
            }
            let data = self.data_lines.join("\n");
            self.data_lines.clear();
            Some(data)
        }
    }
}

pub(crate) fn process_openai_stream_event(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let choice = value.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    let content = delta.get("content")?;
    match content {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text")?.as_str())
                .collect::<String>();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}
