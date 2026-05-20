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
}
