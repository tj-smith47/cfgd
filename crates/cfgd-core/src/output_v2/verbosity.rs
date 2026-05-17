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

impl From<crate::output::Verbosity> for Verbosity {
    fn from(v: crate::output::Verbosity) -> Self {
        match v {
            crate::output::Verbosity::Quiet => Verbosity::Quiet,
            crate::output::Verbosity::Normal => Verbosity::Normal,
            crate::output::Verbosity::Verbose => Verbosity::Verbose,
        }
    }
}

impl From<crate::output::OutputFormat> for OutputFormat {
    fn from(f: crate::output::OutputFormat) -> Self {
        match f {
            crate::output::OutputFormat::Table => OutputFormat::Table,
            crate::output::OutputFormat::Wide => OutputFormat::Wide,
            crate::output::OutputFormat::Json => OutputFormat::Json,
            crate::output::OutputFormat::Yaml => OutputFormat::Yaml,
            crate::output::OutputFormat::Name => OutputFormat::Name,
            crate::output::OutputFormat::Jsonpath(s) => OutputFormat::Jsonpath(s),
            crate::output::OutputFormat::Template(s) => OutputFormat::Template(s),
            crate::output::OutputFormat::TemplateFile(p) => OutputFormat::TemplateFile(p),
        }
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
