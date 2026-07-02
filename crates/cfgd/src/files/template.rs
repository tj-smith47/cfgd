use std::fs;
use std::path::Path;

use tera::{Context, Kwargs, State, Tera};

use cfgd_core::errors::{FileError, Result};

/// Check if a path is a Tera template file (has .tera extension).
pub(crate) fn is_tera_template(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("tera")
}

/// Insert system facts (`__os`, `__arch`, `__hostname`, `__distro`) into a Tera template context.
///
/// - `__os`: operating system (`linux`, `macos`, `freebsd`, `windows`)
/// - `__arch`: CPU architecture (`x86_64`, `aarch64`)
/// - `__hostname`: machine hostname
/// - `__distro`: Linux distribution or pseudo-distro (`ubuntu`, `debian`, `fedora`, `rhel`,
///   `centos`, `arch`, `manjaro`, `alpine`, `opensuse`, `macos`, `freebsd`, `windows`, `unknown`)
pub(super) fn insert_system_facts(ctx: &mut Context) {
    ctx.insert("__os", &std::env::consts::OS);
    ctx.insert("__arch", &std::env::consts::ARCH);
    ctx.insert("__hostname", &cfgd_core::hostname_string());
    ctx.insert(
        "__distro",
        cfgd_core::platform::Platform::detect().distro.as_str(),
    );
}

impl super::CfgdFileManager {
    /// Render a template and return the content for display (e.g., in plan diffs).
    pub fn render_template_for_display(&self, path: &Path) -> Result<String> {
        self.render_template(path, None)
    }

    /// Render a .tera template file with profile env vars and system facts.
    /// If `source_origin` is Some, uses a restricted context with only that
    /// source's env vars — source templates cannot access local env vars.
    pub(super) fn render_template(
        &self,
        path: &Path,
        source_origin: Option<&str>,
    ) -> Result<String> {
        let template_content = fs::read_to_string(path).map_err(|e| FileError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let template_name = path.display().to_string();
        let mut tera = self.tera.lock().map_err(|_| FileError::TemplateError {
            path: path.to_path_buf(),
            message: "tera mutex poisoned".to_string(),
        })?;
        // Register before add_raw_template: tera 2.0 validates function calls during
        // finalize_templates() which is invoked by add_raw_template.
        register_custom_functions(&mut tera, source_origin.is_some());

        tera.add_raw_template(&template_name, &template_content)
            .map_err(|e| FileError::TemplateError {
                path: path.to_path_buf(),
                message: format_tera_error(&e),
            })?;

        // Use source-restricted context if this file came from a source
        let ctx = match source_origin {
            Some(source_name) => self
                .source_contexts
                .get(source_name)
                .unwrap_or(&self.context),
            None => &self.context,
        };

        tera.render(&template_name, ctx).map_err(|e| {
            let msg = format_tera_error(&e);
            // If a source template references an undefined variable, it means
            // it tried to access a local variable that isn't in its sandbox.
            if source_origin.is_some()
                && msg.contains("Variable `")
                && msg.contains("is not defined")
            {
                let var_name = msg
                    .split("Variable `")
                    .nth(1)
                    .and_then(|s| s.split('`').next())
                    .unwrap_or("unknown");
                return cfgd_core::errors::CompositionError::TemplateSandboxViolation {
                    source_name: source_origin.unwrap_or("unknown").to_string(),
                    variable: var_name.to_string(),
                }
                .into();
            }
            FileError::TemplateError {
                path: path.to_path_buf(),
                message: msg,
            }
            .into()
        })
    }
}

/// Format a Tera error with source location details.
pub(super) fn format_tera_error(err: &tera::Error) -> String {
    let mut msg = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        msg.push_str(&format!("\n  caused by: {}", cause));
        source = std::error::Error::source(cause);
    }
    msg
}

/// Register custom Tera functions: os(), hostname(), arch(), env(name).
fn register_custom_functions(tera: &mut Tera, is_source_template: bool) {
    tera.register_function(
        "os",
        |_kwargs: Kwargs, _state: &State| -> tera::TeraResult<tera::Value> {
            Ok(tera::Value::from(std::env::consts::OS))
        },
    );

    tera.register_function(
        "hostname",
        |_kwargs: Kwargs, _state: &State| -> tera::TeraResult<tera::Value> {
            Ok(tera::Value::from(cfgd_core::hostname_string()))
        },
    );

    tera.register_function(
        "arch",
        |_kwargs: Kwargs, _state: &State| -> tera::TeraResult<tera::Value> {
            Ok(tera::Value::from(std::env::consts::ARCH))
        },
    );

    if is_source_template {
        // Source templates are sandboxed: env() is blocked to prevent exfiltration
        // of sensitive environment variables (API keys, credentials, etc.)
        tera.register_function(
            "env",
            |_kwargs: Kwargs, _state: &State| -> tera::TeraResult<tera::Value> {
                Err(tera::Error::message(
                    "env() is not available in source templates (sandbox restriction)",
                ))
            },
        );
    } else {
        tera.register_function(
            "env",
            |kwargs: Kwargs, _state: &State| -> tera::TeraResult<tera::Value> {
                // Match tera 1.x semantics: a missing OR non-string `name`
                // yields the "requires a 'name'" error (get::<&str> rejects
                // non-strings; .ok().flatten() folds that into the None arm).
                let name = kwargs
                    .get::<&str>("name")
                    .ok()
                    .flatten()
                    .ok_or_else(|| tera::Error::message("env() requires a 'name' argument"))?;
                let value = std::env::var(name).unwrap_or_default();
                Ok(tera::Value::from(value))
            },
        );
    }
}
