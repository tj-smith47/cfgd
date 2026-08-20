#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The one log line this binary writes is a JSON document, and its
    // serializer is what makes it safe: `serde_json` escapes every C0 byte —
    // a carriage return, an `ESC` — into visible text before it can reach a
    // terminal through `kubectl logs`. It is NOT routed through cfgd-core's
    // folding writer, which is correct for a plain-text terminal line and
    // wrong here: the fold renders a control character as `\xNN`, which is not
    // a legal JSON escape, so folding a serialized line would put invalid
    // escapes inside its strings and cost every consumer a parseable payload.
    // The residual the serializer leaves is the C1 range (U+0080–U+009F),
    // which it passes through as ordinary UTF-8.
    // default-writer-ok: the JSON serializer is this line's sanitizer, and a fold would put `\xNN` inside its strings
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("info"))
        .json()
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

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    /// The JSON formatter is this binary's sanitizer: a control character in a
    /// value cfgd-csi did not author — a module name off a volume attribute, a
    /// registry's error text — has to reach `kubectl logs` as visible text
    /// rather than as a cursor move, and the line has to stay parseable.
    #[test]
    fn the_json_log_line_neutralizes_a_hostile_field_and_stays_parseable() {
        let poison = format!("nvim{}{}[2Kevil", '\r', '\u{1b}');
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(module = %poison, "cannot publish volume");
        });

        let raw = capture.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let line = String::from_utf8(raw).expect("the formatter writes UTF-8");
        assert!(
            !line.contains('\r') && !line.contains('\u{1b}'),
            "a live control byte reached the log stream: {line:?}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("the line must stay parseable JSON");
        assert_eq!(
            parsed["fields"]["module"], poison,
            "the field must survive the escaping byte-exact: {line:?}"
        );
    }
}
