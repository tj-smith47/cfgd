/// A writer that rewrites every C1 control character (U+0080–U+009F) in a
/// serialized line as its `\u00XX` JSON escape.
///
/// `serde_json` escapes the C0 range and passes C1 through as ordinary UTF-8,
/// and cfgd treats a C1 `U+009B` as the repaint vector it is — 8-bit `CSI`,
/// which a terminal reading `kubectl logs` acts on exactly as it acts on
/// `ESC [`. The escape is chosen over the rest of the workspace's `\xNN` fold
/// precisely because it is legal JSON: the line stays parseable and the field
/// decodes back to the byte-exact value the caller logged.
struct C1Escaping<W>(W);

impl<W: std::io::Write> std::io::Write for C1Escaping<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // UTF-8 encodes this range as `0xC2` followed by the code point's own
        // byte, and a JSON document carries no non-ASCII outside a string, so
        // every such pair is inside one and the replacement is always legal
        // there.
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        while i < buf.len() {
            match (buf[i], buf.get(i + 1)) {
                (0xC2, Some(&c1 @ 0x80..=0x9F)) => {
                    out.extend_from_slice(format!("\\u00{c1:02x}").as_bytes());
                    i += 2;
                }
                _ => {
                    out.push(buf[i]);
                    i += 1;
                }
            }
        }
        self.0.write_all(&out)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The one log line this binary writes is a JSON document, and its
    // serializer is what makes it safe: `serde_json` escapes every C0 byte —
    // a carriage return, an `ESC` — into visible text before it can reach a
    // terminal through `kubectl logs`, and `C1Escaping` closes the C1 range
    // the serializer leaves. It is NOT routed through cfgd-core's folding
    // writer, which is correct for a plain-text terminal line and wrong here:
    // the fold renders a control character as `\xNN`, which is not a legal
    // JSON escape, so folding a serialized line would put invalid escapes
    // inside its strings and cost every consumer a parseable payload.
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("info"))
        .json()
        // unfolded-writer-ok: the JSON serializer plus C1Escaping is this line's sanitizer, and a fold would put `\xNN` inside its strings
        .with_writer(|| C1Escaping(std::io::stdout()))
        .init();
    cfgd_csi::app::run().await
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    /// A capture standing in for the stream the subscriber writes.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).extend(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The JSON formatter plus `C1Escaping` is this binary's sanitizer: a
    /// control character in a value cfgd-csi did not author — a module name off
    /// a volume attribute, a registry's error text — has to reach
    /// `kubectl logs` as visible text rather than as a cursor move, and the
    /// line has to stay parseable. The C1 half is the one `serde_json` does not
    /// answer on its own.
    #[test]
    fn the_json_log_line_neutralizes_a_hostile_field_and_stays_parseable() {
        let poison = format!("nvim{}{}[2Kevil{}[2Kworse", '\r', '\u{1b}', '\u{9b}');
        let capture = Capture::default();
        let sink = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            // unfolded-writer-ok: the production writer under a capture, which is what this test exists to exercise
            .with_writer(move || super::C1Escaping(sink.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(module = %poison, "cannot publish volume");
        });

        let raw = capture.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let line = String::from_utf8(raw).expect("the formatter writes UTF-8");
        assert!(
            !line.contains('\r') && !line.contains('\u{1b}') && !line.contains('\u{9b}'),
            "a live control byte reached the log stream: {line:?}"
        );
        assert!(
            line.contains("\\u009b"),
            "the C1 byte must survive as its JSON escape: {line:?}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("the line must stay parseable JSON");
        assert_eq!(
            parsed["fields"]["module"], poison,
            "the field must survive the escaping byte-exact: {line:?}"
        );
    }
}
