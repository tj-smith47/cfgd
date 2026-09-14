use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Wide,
    Json,
    Yaml,
    Name,
    Jsonpath(String),
    Template(String),
    TemplateFile(PathBuf),
}

impl OutputFormat {
    /// True when the format expects machine-consumable structured output.
    /// Used to auto-quiet status output and to refuse interactive prompts.
    pub fn is_structured(&self) -> bool {
        !matches!(self, OutputFormat::Table | OutputFormat::Wide)
    }

    /// Whether this format's payload refuses colour outright.
    ///
    /// Narrower than [`Self::is_structured`]: an escape inside a JSON string
    /// field, a `-o name` line a shell reads back, or a jsonpath/template
    /// projection is corrupt data, so those four never carry one. YAML is the
    /// exception — the bytes are a document a person reads as often as a script
    /// parses, so it follows the ordinary colour decision and highlights when
    /// that decision is on.
    pub fn refuses_color(&self) -> bool {
        matches!(
            self,
            OutputFormat::Json
                | OutputFormat::Name
                | OutputFormat::Jsonpath(_)
                | OutputFormat::Template(_)
                | OutputFormat::TemplateFile(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_modes_classified() {
        assert!(!OutputFormat::Table.is_structured());
        assert!(!OutputFormat::Wide.is_structured());
        assert!(OutputFormat::Json.is_structured());
        assert!(OutputFormat::Yaml.is_structured());
        assert!(OutputFormat::Name.is_structured());
        assert!(OutputFormat::Jsonpath("{.foo}".into()).is_structured());
    }

    #[test]
    fn only_yaml_among_the_structured_formats_accepts_colour() {
        assert!(!OutputFormat::Yaml.refuses_color());
        assert!(!OutputFormat::Table.refuses_color());
        assert!(!OutputFormat::Wide.refuses_color());
        for refusing in [
            OutputFormat::Json,
            OutputFormat::Name,
            OutputFormat::Jsonpath("{.foo}".into()),
            OutputFormat::Template("{{.foo}}".into()),
            OutputFormat::TemplateFile("t.tmpl".into()),
        ] {
            assert!(
                refusing.refuses_color(),
                "{refusing:?} carries a machine contract and must refuse colour"
            );
        }
    }
}
