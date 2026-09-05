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
        cfgd_core::platform::Platform::current().distro.as_str(),
    );
}

/// One Tera engine plus the bodies already registered in it.
///
/// `add_raw_template` re-runs `finalize_templates` over EVERY template the
/// engine holds, so re-registering a body already in there costs a walk of the
/// whole set — and a run renders the same file repeatedly (plan previews it,
/// apply writes it, a diff shows it again). `registered` maps each template
/// name to the digest of the body last stored under it, so an unchanged body
/// is skipped and a body that changed under the same name (a `preApply` hook
/// that rewrites a source template) is still re-registered rather than
/// rendered stale.
struct TemplateEngine {
    tera: Tera,
    registered: std::collections::HashMap<String, String>,
    /// How many bodies this engine has actually handed to `add_raw_template`.
    /// The observable behind the skip: a render that reused a registration
    /// leaves it where a re-registration would have raised it.
    #[cfg(test)]
    registrations: usize,
}

impl TemplateEngine {
    fn new(sandboxed: bool) -> Self {
        let mut tera = Tera::default();
        tera.autoescape_on(Vec::<&str>::new());
        register_custom_functions(&mut tera, sandboxed);
        Self {
            tera,
            registered: std::collections::HashMap::new(),
            #[cfg(test)]
            registrations: 0,
        }
    }

    fn ensure_template(&mut self, name: &str, content: &str) -> tera::TeraResult<()> {
        let digest = cfgd_core::sha256_hex(content.as_bytes());
        if self.registered.get(name).is_some_and(|d| *d == digest) {
            return Ok(());
        }
        self.tera.add_raw_template(name, content)?;
        self.registered.insert(name.to_string(), digest);
        #[cfg(test)]
        {
            self.registrations += 1;
        }
        Ok(())
    }
}

/// The two engines a file manager renders through, split by sandbox.
///
/// The ONLY per-render difference in the function set is whether `env()` is
/// blocked, and a single engine could serve both only by re-registering every
/// custom function immediately before each render. Two engines register each
/// function exactly once for the manager's whole life and make the sandbox a
/// property of which engine a template lands in rather than of what the last
/// caller happened to install.
///
/// The split is also the reach boundary for Tera's own `include` / `extends`:
/// a template resolves only names registered in the SAME engine, so a local
/// template cannot pull in a source-delivered one, or the reverse. That is the
/// intended shape — a source-delivered template inheriting from a local one
/// would render sandboxed content through an unsandboxed parent — but it means
/// a cross-origin reference is unrepresentable rather than merely denied.
pub(super) struct TemplateEngines {
    local: TemplateEngine,
    sandboxed: TemplateEngine,
}

impl TemplateEngines {
    pub(super) fn new() -> Self {
        Self {
            local: TemplateEngine::new(false),
            sandboxed: TemplateEngine::new(true),
        }
    }

    /// Total `add_raw_template` calls across both engines.
    #[cfg(test)]
    pub(super) fn registrations(&self) -> usize {
        self.local.registrations + self.sandboxed.registrations
    }

    fn engine_for(&mut self, source_origin: Option<&str>) -> &mut TemplateEngine {
        if source_origin.is_some() {
            &mut self.sandboxed
        } else {
            &mut self.local
        }
    }
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
        let mut engines = self.tera.lock().map_err(|_| FileError::TemplateError {
            path: path.to_path_buf(),
            message: "tera mutex poisoned".to_string(),
        })?;
        // The engine's custom functions were registered when it was built, which
        // tera 2.0 requires to happen before `add_raw_template` — that call runs
        // `finalize_templates()`, which validates every function a template
        // calls.
        let engine = engines.engine_for(source_origin);
        engine
            .ensure_template(&template_name, &template_content)
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

        engine.tera.render(&template_name, ctx).map_err(|e| {
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
