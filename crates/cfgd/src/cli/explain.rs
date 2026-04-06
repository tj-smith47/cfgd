use serde::Serialize;

use super::*;

// cfgd explain — schema documentation for all resource types
// ---------------------------------------------------------------------------

/// A field in a resource schema.
struct SchemaField {
    /// YAML field name (camelCase)
    name: &'static str,
    /// Field type description
    type_desc: &'static str,
    /// Whether the field is required
    required: bool,
    /// Short description
    description: &'static str,
    /// Nested fields (for objects)
    children: &'static [SchemaField],
}

/// A top-level resource type.
struct ResourceSchema {
    /// Display name
    name: &'static str,
    /// apiVersion value
    api_version: &'static str,
    /// kind value
    kind: &'static str,
    /// File location hint
    location: &'static str,
    /// Short description
    description: &'static str,
    /// Top-level fields under spec (or root for non-KRM)
    fields: &'static [SchemaField],
}

// --- Schema definitions (compile-time embedded) ---

static SCHEMA_CONFIG: ResourceSchema = ResourceSchema {
    name: "CfgdConfig",
    api_version: cfgd_core::API_VERSION,
    kind: "Config",
    location: "cfgd.yaml",
    description: "Root configuration file for cfgd. Defines the active profile, origin, daemon settings, secrets backend, sources, theme, and module sources.",
    fields: &[
        SchemaField {
            name: "profile",
            type_desc: "string",
            required: true,
            description: "Active profile name for this machine",
            children: &[],
        },
        SchemaField {
            name: "origin",
            type_desc: "object | []object",
            required: false,
            description: "Git or server origin(s) for config syncing",
            children: &[
                SchemaField {
                    name: "type",
                    type_desc: "string",
                    required: true,
                    description: "Origin type: Git | Server",
                    children: &[],
                },
                SchemaField {
                    name: "url",
                    type_desc: "string",
                    required: true,
                    description: "Repository URL or server endpoint",
                    children: &[],
                },
                SchemaField {
                    name: "branch",
                    type_desc: "string",
                    required: false,
                    description: "Git branch (default: master)",
                    children: &[],
                },
                SchemaField {
                    name: "auth",
                    type_desc: "string",
                    required: false,
                    description: "Auth method for server origins (e.g., device-flow)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "daemon",
            type_desc: "object",
            required: false,
            description: "Daemon configuration",
            children: &[
                SchemaField {
                    name: "enabled",
                    type_desc: "bool",
                    required: false,
                    description: "Enable the daemon (default: false)",
                    children: &[],
                },
                SchemaField {
                    name: "reconcile",
                    type_desc: "object",
                    required: false,
                    description: "Reconciliation settings",
                    children: &[
                        SchemaField {
                            name: "interval",
                            type_desc: "string",
                            required: false,
                            description: "Reconciliation interval (default: 5m)",
                            children: &[],
                        },
                        SchemaField {
                            name: "onChange",
                            type_desc: "bool",
                            required: false,
                            description: "Reconcile on file changes (default: false)",
                            children: &[],
                        },
                        SchemaField {
                            name: "autoApply",
                            type_desc: "bool",
                            required: false,
                            description: "Auto-apply on reconcile (default: false)",
                            children: &[],
                        },
                        SchemaField {
                            name: "policy",
                            type_desc: "object",
                            required: false,
                            description: "Auto-apply policy for source updates",
                            children: &[
                                SchemaField {
                                    name: "newRecommended",
                                    type_desc: "string",
                                    required: false,
                                    description: "Action for new recommended items: Notify | Accept | Reject | Ignore (default: Notify)",
                                    children: &[],
                                },
                                SchemaField {
                                    name: "newOptional",
                                    type_desc: "string",
                                    required: false,
                                    description: "Action for new optional items (default: ignore)",
                                    children: &[],
                                },
                                SchemaField {
                                    name: "lockedConflict",
                                    type_desc: "string",
                                    required: false,
                                    description: "Action for locked conflicts (default: Notify)",
                                    children: &[],
                                },
                            ],
                        },
                    ],
                },
                SchemaField {
                    name: "sync",
                    type_desc: "object",
                    required: false,
                    description: "Sync settings for git origin",
                    children: &[
                        SchemaField {
                            name: "autoPush",
                            type_desc: "bool",
                            required: false,
                            description: "Auto-push local changes to remote",
                            children: &[],
                        },
                        SchemaField {
                            name: "autoPull",
                            type_desc: "bool",
                            required: false,
                            description: "Auto-pull remote changes",
                            children: &[],
                        },
                        SchemaField {
                            name: "interval",
                            type_desc: "string",
                            required: false,
                            description: "Sync interval (default: 1h)",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "notify",
                    type_desc: "object",
                    required: false,
                    description: "Notification settings",
                    children: &[
                        SchemaField {
                            name: "drift",
                            type_desc: "bool",
                            required: false,
                            description: "Notify on drift detection",
                            children: &[],
                        },
                        SchemaField {
                            name: "method",
                            type_desc: "string",
                            required: false,
                            description: "Notification method: Desktop | Stdout | Webhook (default: Desktop)",
                            children: &[],
                        },
                        SchemaField {
                            name: "webhookUrl",
                            type_desc: "string",
                            required: false,
                            description: "Webhook URL for webhook notifications",
                            children: &[],
                        },
                    ],
                },
            ],
        },
        SchemaField {
            name: "secrets",
            type_desc: "object",
            required: false,
            description: "Secrets backend configuration",
            children: &[
                SchemaField {
                    name: "backend",
                    type_desc: "string",
                    required: false,
                    description: "Secrets backend: sops | age (default: sops)",
                    children: &[],
                },
                SchemaField {
                    name: "sops",
                    type_desc: "object",
                    required: false,
                    description: "SOPS-specific configuration",
                    children: &[SchemaField {
                        name: "ageKey",
                        type_desc: "string",
                        required: false,
                        description: "Path to age key file",
                        children: &[],
                    }],
                },
                SchemaField {
                    name: "integrations",
                    type_desc: "[]object",
                    required: false,
                    description: "External secret provider integrations",
                    children: &[SchemaField {
                        name: "name",
                        type_desc: "string",
                        required: true,
                        description: "Provider name: 1password | bitwarden | vault",
                        children: &[],
                    }],
                },
            ],
        },
        SchemaField {
            name: "sources",
            type_desc: "[]object",
            required: false,
            description: "Multi-source config subscriptions",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Source name",
                    children: &[],
                },
                SchemaField {
                    name: "origin",
                    type_desc: "object",
                    required: true,
                    description: "Source origin (same schema as top-level origin)",
                    children: &[
                        SchemaField {
                            name: "type",
                            type_desc: "string",
                            required: true,
                            description: "Origin type: Git | Server",
                            children: &[],
                        },
                        SchemaField {
                            name: "url",
                            type_desc: "string",
                            required: true,
                            description: "Repository URL or server endpoint",
                            children: &[],
                        },
                        SchemaField {
                            name: "branch",
                            type_desc: "string",
                            required: false,
                            description: "Git branch (default: master)",
                            children: &[],
                        },
                        SchemaField {
                            name: "auth",
                            type_desc: "string",
                            required: false,
                            description: "Auth method for server origins (e.g., device-flow)",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "subscription",
                    type_desc: "object",
                    required: false,
                    description: "Subscription preferences",
                    children: &[
                        SchemaField {
                            name: "profile",
                            type_desc: "string",
                            required: false,
                            description: "Profile to subscribe to from the source",
                            children: &[],
                        },
                        SchemaField {
                            name: "priority",
                            type_desc: "integer",
                            required: false,
                            description: "Merge priority (default: 500, local: 1000)",
                            children: &[],
                        },
                        SchemaField {
                            name: "acceptRecommended",
                            type_desc: "bool",
                            required: false,
                            description: "Auto-accept recommended items",
                            children: &[],
                        },
                        SchemaField {
                            name: "optIn",
                            type_desc: "[]string",
                            required: false,
                            description: "Optional items to opt into",
                            children: &[],
                        },
                        SchemaField {
                            name: "overrides",
                            type_desc: "object",
                            required: false,
                            description: "Override values from the source",
                            children: &[],
                        },
                        SchemaField {
                            name: "reject",
                            type_desc: "object",
                            required: false,
                            description: "Reject specific items from the source",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "sync",
                    type_desc: "object",
                    required: false,
                    description: "Source sync settings",
                    children: &[
                        SchemaField {
                            name: "interval",
                            type_desc: "string",
                            required: false,
                            description: "Sync interval (default: 1h)",
                            children: &[],
                        },
                        SchemaField {
                            name: "autoApply",
                            type_desc: "bool",
                            required: false,
                            description: "Auto-apply source updates",
                            children: &[],
                        },
                        SchemaField {
                            name: "pinVersion",
                            type_desc: "string",
                            required: false,
                            description: "Pin source to a semver range",
                            children: &[],
                        },
                    ],
                },
            ],
        },
        SchemaField {
            name: "theme",
            type_desc: "string | object",
            required: false,
            description: "Theme name (string) or theme config (object with name + overrides)",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: false,
                    description: "Theme name: default | dracula | solarized-dark | solarized-light | minimal",
                    children: &[],
                },
                SchemaField {
                    name: "overrides",
                    type_desc: "object",
                    required: false,
                    description: "Override individual theme colors and icons",
                    children: &[
                        SchemaField {
                            name: "success",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for success styling (e.g., #50fa7b)",
                            children: &[],
                        },
                        SchemaField {
                            name: "warning",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for warning styling",
                            children: &[],
                        },
                        SchemaField {
                            name: "error",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for error styling",
                            children: &[],
                        },
                        SchemaField {
                            name: "info",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for info styling",
                            children: &[],
                        },
                        SchemaField {
                            name: "muted",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for muted/dim text",
                            children: &[],
                        },
                        SchemaField {
                            name: "header",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for header text",
                            children: &[],
                        },
                        SchemaField {
                            name: "subheader",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for subheader text",
                            children: &[],
                        },
                        SchemaField {
                            name: "key",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for key text in key-value pairs",
                            children: &[],
                        },
                        SchemaField {
                            name: "value",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for value text in key-value pairs",
                            children: &[],
                        },
                        SchemaField {
                            name: "diffAdd",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for diff additions",
                            children: &[],
                        },
                        SchemaField {
                            name: "diffRemove",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for diff removals",
                            children: &[],
                        },
                        SchemaField {
                            name: "diffContext",
                            type_desc: "string",
                            required: false,
                            description: "Hex color for diff context lines",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconSuccess",
                            type_desc: "string",
                            required: false,
                            description: "Custom success icon (default: ✓)",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconWarning",
                            type_desc: "string",
                            required: false,
                            description: "Custom warning icon (default: ⚠)",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconError",
                            type_desc: "string",
                            required: false,
                            description: "Custom error icon (default: ✗)",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconInfo",
                            type_desc: "string",
                            required: false,
                            description: "Custom info icon (default: ●)",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconPending",
                            type_desc: "string",
                            required: false,
                            description: "Custom pending icon (default: ○)",
                            children: &[],
                        },
                        SchemaField {
                            name: "iconArrow",
                            type_desc: "string",
                            required: false,
                            description: "Custom arrow icon (default: →)",
                            children: &[],
                        },
                    ],
                },
            ],
        },
        SchemaField {
            name: "modules",
            type_desc: "object",
            required: false,
            description: "Module configuration: registries and security",
            children: &[
                SchemaField {
                    name: "registries",
                    type_desc: "[]object",
                    required: false,
                    description: "Module registries — searchable indexes of reusable modules",
                    children: &[
                        SchemaField {
                            name: "name",
                            type_desc: "string",
                            required: true,
                            description: "Short name/alias for this registry",
                            children: &[],
                        },
                        SchemaField {
                            name: "url",
                            type_desc: "string",
                            required: true,
                            description: "Git URL of the registry repository",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "security",
                    type_desc: "object",
                    required: false,
                    description: "Module security settings",
                    children: &[SchemaField {
                        name: "requireSignatures",
                        type_desc: "bool",
                        required: false,
                        description: "Require GPG/SSH signatures on remote module tags (default: false)",
                        children: &[],
                    }],
                },
            ],
        },
    ],
};

static SCHEMA_PROFILE: ResourceSchema = ResourceSchema {
    name: "Profile",
    api_version: cfgd_core::API_VERSION,
    kind: "Profile",
    location: "profiles/<name>.yaml",
    description: "Defines the desired state for a machine: packages, files, system settings, secrets, and scripts. Supports inheritance for layered configuration.",
    fields: &[
        SchemaField {
            name: "inherits",
            type_desc: "[]string",
            required: false,
            description: "Ordered list of parent profiles (later overrides earlier)",
            children: &[],
        },
        SchemaField {
            name: "modules",
            type_desc: "[]string",
            required: false,
            description: "Modules to include (local names or registry/module references)",
            children: &[],
        },
        SchemaField {
            name: "env",
            type_desc: "list[{name, value}]",
            required: false,
            description: "Environment variables (name/value pairs) available in templates and written to ~/.cfgd.env",
            children: &[],
        },
        SchemaField {
            name: "aliases",
            type_desc: "list[{name, command}]",
            required: false,
            description: "Shell aliases written to ~/.cfgd.env as `alias name=\"command\"` (fish: `abbr -a name command`)",
            children: &[],
        },
        SchemaField {
            name: "packages",
            type_desc: "object",
            required: false,
            description: "Package declarations by manager",
            children: &[
                SchemaField {
                    name: "brew",
                    type_desc: "object",
                    required: false,
                    description: "Homebrew packages",
                    children: &[
                        SchemaField {
                            name: "file",
                            type_desc: "string",
                            required: false,
                            description: "Path to Brewfile (relative to config repo root)",
                            children: &[],
                        },
                        SchemaField {
                            name: "taps",
                            type_desc: "[]string",
                            required: false,
                            description: "Homebrew taps to add",
                            children: &[],
                        },
                        SchemaField {
                            name: "formulae",
                            type_desc: "[]string",
                            required: false,
                            description: "Homebrew formulae to install",
                            children: &[],
                        },
                        SchemaField {
                            name: "casks",
                            type_desc: "[]string",
                            required: false,
                            description: "Homebrew casks to install",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "apt",
                    type_desc: "object",
                    required: false,
                    description: "APT packages (Debian/Ubuntu)",
                    children: &[
                        SchemaField {
                            name: "file",
                            type_desc: "string",
                            required: false,
                            description: "Path to package list file (one per line)",
                            children: &[],
                        },
                        SchemaField {
                            name: "packages",
                            type_desc: "[]string",
                            required: false,
                            description: "Packages to install",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "cargo",
                    type_desc: "[]string | object",
                    required: false,
                    description: "Cargo/Rust packages",
                    children: &[
                        SchemaField {
                            name: "file",
                            type_desc: "string",
                            required: false,
                            description: "Path to Cargo.toml to read dependencies from",
                            children: &[],
                        },
                        SchemaField {
                            name: "packages",
                            type_desc: "[]string",
                            required: false,
                            description: "Packages to install",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "npm",
                    type_desc: "object",
                    required: false,
                    description: "NPM global packages",
                    children: &[
                        SchemaField {
                            name: "file",
                            type_desc: "string",
                            required: false,
                            description: "Path to package.json",
                            children: &[],
                        },
                        SchemaField {
                            name: "global",
                            type_desc: "[]string",
                            required: false,
                            description: "Global packages to install",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "pipx",
                    type_desc: "[]string",
                    required: false,
                    description: "pipx Python packages",
                    children: &[],
                },
                SchemaField {
                    name: "dnf",
                    type_desc: "[]string",
                    required: false,
                    description: "DNF packages (Fedora/RHEL)",
                    children: &[],
                },
                SchemaField {
                    name: "apk",
                    type_desc: "[]string",
                    required: false,
                    description: "APK packages (Alpine)",
                    children: &[],
                },
                SchemaField {
                    name: "pacman",
                    type_desc: "[]string",
                    required: false,
                    description: "Pacman packages (Arch/Manjaro)",
                    children: &[],
                },
                SchemaField {
                    name: "zypper",
                    type_desc: "[]string",
                    required: false,
                    description: "Zypper packages (openSUSE/SLES)",
                    children: &[],
                },
                SchemaField {
                    name: "yum",
                    type_desc: "[]string",
                    required: false,
                    description: "Yum packages (RHEL 7/CentOS 7)",
                    children: &[],
                },
                SchemaField {
                    name: "pkg",
                    type_desc: "[]string",
                    required: false,
                    description: "pkg packages (FreeBSD)",
                    children: &[],
                },
                SchemaField {
                    name: "snap",
                    type_desc: "object",
                    required: false,
                    description: "Snap packages (Ubuntu)",
                    children: &[
                        SchemaField {
                            name: "packages",
                            type_desc: "[]string",
                            required: false,
                            description: "Snap packages to install",
                            children: &[],
                        },
                        SchemaField {
                            name: "classic",
                            type_desc: "[]string",
                            required: false,
                            description: "Snap packages to install with --classic",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "flatpak",
                    type_desc: "object",
                    required: false,
                    description: "Flatpak packages",
                    children: &[
                        SchemaField {
                            name: "packages",
                            type_desc: "[]string",
                            required: false,
                            description: "Flatpak app IDs (reverse-DNS)",
                            children: &[],
                        },
                        SchemaField {
                            name: "remote",
                            type_desc: "string",
                            required: false,
                            description: "Flatpak remote (default: flathub)",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "nix",
                    type_desc: "[]string",
                    required: false,
                    description: "Nix packages",
                    children: &[],
                },
                SchemaField {
                    name: "go",
                    type_desc: "[]string",
                    required: false,
                    description: "Go packages (go install)",
                    children: &[],
                },
                SchemaField {
                    name: "winget",
                    type_desc: "[]string",
                    required: false,
                    description: "Windows Package Manager (winget) packages by ID (Windows only)",
                    children: &[],
                },
                SchemaField {
                    name: "chocolatey",
                    type_desc: "[]string",
                    required: false,
                    description: "Chocolatey packages (Windows only)",
                    children: &[],
                },
                SchemaField {
                    name: "scoop",
                    type_desc: "[]string",
                    required: false,
                    description: "Scoop packages (Windows only)",
                    children: &[],
                },
                SchemaField {
                    name: "custom",
                    type_desc: "[]object",
                    required: false,
                    description: "User-defined package managers",
                    children: &[
                        SchemaField {
                            name: "name",
                            type_desc: "string",
                            required: true,
                            description: "Manager name",
                            children: &[],
                        },
                        SchemaField {
                            name: "check",
                            type_desc: "string",
                            required: true,
                            description: "Shell command to check availability",
                            children: &[],
                        },
                        SchemaField {
                            name: "listInstalled",
                            type_desc: "string",
                            required: true,
                            description: "Shell command to list installed packages (one per line)",
                            children: &[],
                        },
                        SchemaField {
                            name: "install",
                            type_desc: "string",
                            required: true,
                            description: "Shell command template to install ({packages} or {package})",
                            children: &[],
                        },
                        SchemaField {
                            name: "uninstall",
                            type_desc: "string",
                            required: true,
                            description: "Shell command template to uninstall",
                            children: &[],
                        },
                        SchemaField {
                            name: "update",
                            type_desc: "string",
                            required: false,
                            description: "Shell command to update all packages",
                            children: &[],
                        },
                        SchemaField {
                            name: "packages",
                            type_desc: "[]string",
                            required: false,
                            description: "Packages to manage",
                            children: &[],
                        },
                    ],
                },
            ],
        },
        SchemaField {
            name: "files",
            type_desc: "object",
            required: false,
            description: "File management declarations",
            children: &[
                SchemaField {
                    name: "managed",
                    type_desc: "[]object",
                    required: false,
                    description: "Files to manage",
                    children: &[
                        SchemaField {
                            name: "source",
                            type_desc: "string",
                            required: true,
                            description: "Relative path in source repo",
                            children: &[],
                        },
                        SchemaField {
                            name: "target",
                            type_desc: "string",
                            required: true,
                            description: "Absolute target path on the machine",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "permissions",
                    type_desc: "map[string]string",
                    required: false,
                    description: "Permission overrides by path (e.g., \".ssh/config\": \"600\")",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "system",
            type_desc: "map[string]any",
            required: false,
            description: "System configurator settings (keys map to registered configurators)",
            children: &[
                SchemaField {
                    name: "shell",
                    type_desc: "string",
                    required: false,
                    description: "Default shell path (e.g., /bin/zsh)",
                    children: &[],
                },
                SchemaField {
                    name: "macosDefaults",
                    type_desc: "map[string]map",
                    required: false,
                    description: "macOS defaults by domain and key",
                    children: &[],
                },
                SchemaField {
                    name: "launchAgents",
                    type_desc: "[]object",
                    required: false,
                    description: "macOS LaunchAgent definitions",
                    children: &[],
                },
                SchemaField {
                    name: "systemdUnits",
                    type_desc: "[]object",
                    required: false,
                    description: "systemd unit file management",
                    children: &[],
                },
                SchemaField {
                    name: "gsettings",
                    type_desc: "map[string]map",
                    required: false,
                    description: "GNOME/GTK desktop settings by schema and key (Linux)",
                    children: &[],
                },
                SchemaField {
                    name: "kdeConfig",
                    type_desc: "map[string]map[string]map",
                    required: false,
                    description: "KDE Plasma settings by file, group, and key (Linux)",
                    children: &[],
                },
                SchemaField {
                    name: "xfconf",
                    type_desc: "map[string]map",
                    required: false,
                    description: "XFCE desktop settings by channel and property (Linux)",
                    children: &[],
                },
                SchemaField {
                    name: "environment",
                    type_desc: "map[string]string",
                    required: false,
                    description: "Environment variable declarations",
                    children: &[],
                },
                SchemaField {
                    name: "sysctl",
                    type_desc: "map[string]any",
                    required: false,
                    description: "Kernel parameters (Linux nodes)",
                    children: &[],
                },
                SchemaField {
                    name: "kernelModules",
                    type_desc: "[]string",
                    required: false,
                    description: "Kernel modules to load (Linux nodes)",
                    children: &[],
                },
                SchemaField {
                    name: "containerd",
                    type_desc: "object",
                    required: false,
                    description: "containerd configuration (k8s nodes)",
                    children: &[],
                },
                SchemaField {
                    name: "kubelet",
                    type_desc: "object",
                    required: false,
                    description: "kubelet configuration (k8s nodes)",
                    children: &[],
                },
                SchemaField {
                    name: "windowsRegistry",
                    type_desc: "map[string]map",
                    required: false,
                    description: "Windows registry settings (Windows only). Keys are registry paths, values are name-value maps",
                    children: &[],
                },
                SchemaField {
                    name: "windowsServices",
                    type_desc: "[]object",
                    required: false,
                    description: "Windows Service management (Windows only). Each entry has name, binaryPath, startType, state",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "secrets",
            type_desc: "[]object",
            required: false,
            description: "Secret file declarations",
            children: &[
                SchemaField {
                    name: "source",
                    type_desc: "string",
                    required: true,
                    description: "Path to SOPS-encrypted file or provider://ref",
                    children: &[],
                },
                SchemaField {
                    name: "target",
                    type_desc: "string",
                    required: true,
                    description: "Target path for decrypted output",
                    children: &[],
                },
                SchemaField {
                    name: "template",
                    type_desc: "string",
                    required: false,
                    description: "Template string to inject secret into",
                    children: &[],
                },
                SchemaField {
                    name: "backend",
                    type_desc: "string",
                    required: false,
                    description: "Override secrets backend: sops | age",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "scripts",
            type_desc: "object",
            required: false,
            description: "Lifecycle scripts",
            children: &[
                SchemaField {
                    name: "preReconcile",
                    type_desc: "[]string",
                    required: false,
                    description: "Scripts to run before reconciliation",
                    children: &[],
                },
                SchemaField {
                    name: "postReconcile",
                    type_desc: "[]string",
                    required: false,
                    description: "Scripts to run after reconciliation",
                    children: &[],
                },
            ],
        },
    ],
};

static SCHEMA_MODULE: ResourceSchema = ResourceSchema {
    name: "Module",
    api_version: cfgd_core::API_VERSION,
    kind: "Module",
    location: "modules/<name>/module.yaml",
    description: "Self-contained, portable configuration unit. Defines packages, files, and scripts with cross-platform resolution and dependency management.",
    fields: &[
        SchemaField {
            name: "depends",
            type_desc: "[]string",
            required: false,
            description: "Module dependencies (resolved via topological sort)",
            children: &[],
        },
        SchemaField {
            name: "packages",
            type_desc: "[]object",
            required: false,
            description: "Platform-agnostic package declarations",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Canonical package name",
                    children: &[],
                },
                SchemaField {
                    name: "minVersion",
                    type_desc: "string",
                    required: false,
                    description: "Minimum required version (semver)",
                    children: &[],
                },
                SchemaField {
                    name: "prefer",
                    type_desc: "[]string",
                    required: false,
                    description: "Preferred package managers in order (e.g., [brew, apt, script])",
                    children: &[],
                },
                SchemaField {
                    name: "aliases",
                    type_desc: "map[string]string",
                    required: false,
                    description: "Manager-specific package names (e.g., {apt: fd-find})",
                    children: &[],
                },
                SchemaField {
                    name: "script",
                    type_desc: "string",
                    required: false,
                    description: "Inline install script or path (used when prefer includes 'script')",
                    children: &[],
                },
                SchemaField {
                    name: "platforms",
                    type_desc: "[]string",
                    required: false,
                    description: "Platform filter: linux, macos, freebsd, ubuntu, arch, x86_64, aarch64",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "files",
            type_desc: "[]object",
            required: false,
            description: "Files to manage (local or git sources)",
            children: &[
                SchemaField {
                    name: "source",
                    type_desc: "string",
                    required: true,
                    description: "Source path (local relative or git URL with @tag, ?ref=, //subdir)",
                    children: &[],
                },
                SchemaField {
                    name: "target",
                    type_desc: "string",
                    required: true,
                    description: "Target path on the machine",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "env",
            type_desc: "list[{name, value}]",
            required: false,
            description: "Environment variables merged with profile env (module wins on conflict by name)",
            children: &[],
        },
        SchemaField {
            name: "aliases",
            type_desc: "list[{name, command}]",
            required: false,
            description: "Shell aliases merged with profile aliases (module wins on conflict by name)",
            children: &[],
        },
        SchemaField {
            name: "scripts",
            type_desc: "object",
            required: false,
            description: "Lifecycle scripts",
            children: &[SchemaField {
                name: "postApply",
                type_desc: "[]string",
                required: false,
                description: "Scripts to run after module is applied",
                children: &[],
            }],
        },
    ],
};

static SCHEMA_CONFIG_SOURCE: ResourceSchema = ResourceSchema {
    name: "ConfigSource",
    api_version: cfgd_core::API_VERSION,
    kind: "ConfigSource",
    location: "cfgd-source.yaml (in source repo root)",
    description: "Team config source manifest. Published by teams in their config repos to define profiles, modules, and policy tiers available for subscription.",
    fields: &[
        SchemaField {
            name: "provides",
            type_desc: "object",
            required: false,
            description: "What this source provides",
            children: &[
                SchemaField {
                    name: "profiles",
                    type_desc: "[]string",
                    required: false,
                    description: "Profile names available from this source",
                    children: &[],
                },
                SchemaField {
                    name: "profileDetails",
                    type_desc: "[]object",
                    required: false,
                    description: "Detailed profile entries with descriptions",
                    children: &[
                        SchemaField {
                            name: "name",
                            type_desc: "string",
                            required: true,
                            description: "Profile name",
                            children: &[],
                        },
                        SchemaField {
                            name: "description",
                            type_desc: "string",
                            required: false,
                            description: "Profile description",
                            children: &[],
                        },
                        SchemaField {
                            name: "path",
                            type_desc: "string",
                            required: false,
                            description: "Path to profile YAML",
                            children: &[],
                        },
                        SchemaField {
                            name: "inherits",
                            type_desc: "[]string",
                            required: false,
                            description: "Profiles this inherits from",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "platformProfiles",
                    type_desc: "map[string]string",
                    required: false,
                    description: "OS/distro to profile mapping for auto-detection",
                    children: &[],
                },
                SchemaField {
                    name: "modules",
                    type_desc: "[]string",
                    required: false,
                    description: "Module names available from this source",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "policy",
            type_desc: "object",
            required: false,
            description: "Policy tiers controlling how items are applied",
            children: &[
                SchemaField {
                    name: "required",
                    type_desc: "object",
                    required: false,
                    description: "Items that must be applied (enforced)",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "recommended",
                    type_desc: "object",
                    required: false,
                    description: "Items that are recommended (prompted)",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "optional",
                    type_desc: "object",
                    required: false,
                    description: "Items that are opt-in",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "locked",
                    type_desc: "object",
                    required: false,
                    description: "Items that cannot be overridden by subscribers",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "constraints",
                    type_desc: "object",
                    required: false,
                    description: "Security constraints on source capabilities",
                    children: &[
                        SchemaField {
                            name: "noScripts",
                            type_desc: "bool",
                            required: false,
                            description: "Disallow scripts from this source (default: true)",
                            children: &[],
                        },
                        SchemaField {
                            name: "noSecretsRead",
                            type_desc: "bool",
                            required: false,
                            description: "Disallow secret reading (default: true)",
                            children: &[],
                        },
                        SchemaField {
                            name: "allowedTargetPaths",
                            type_desc: "[]string",
                            required: false,
                            description: "Restrict file targets to these path prefixes",
                            children: &[],
                        },
                        SchemaField {
                            name: "allowSystemChanges",
                            type_desc: "bool",
                            required: false,
                            description: "Allow system configurator changes (default: false)",
                            children: &[],
                        },
                    ],
                },
            ],
        },
    ],
};

static POLICY_ITEMS_FIELDS: [SchemaField; 6] = [
    SchemaField {
        name: "packages",
        type_desc: "object",
        required: false,
        description: "Package declarations (same schema as profile packages)",
        children: &[],
    },
    SchemaField {
        name: "files",
        type_desc: "[]object",
        required: false,
        description: "Managed file declarations",
        children: &[],
    },
    SchemaField {
        name: "env",
        type_desc: "list[{name, value}]",
        required: false,
        description: "Environment variable declarations",
        children: &[],
    },
    SchemaField {
        name: "system",
        type_desc: "map[string]any",
        required: false,
        description: "System configurator settings",
        children: &[],
    },
    SchemaField {
        name: "profiles",
        type_desc: "[]string",
        required: false,
        description: "Profiles in this tier",
        children: &[],
    },
    SchemaField {
        name: "modules",
        type_desc: "[]string",
        required: false,
        description: "Modules in this tier",
        children: &[],
    },
];

static SCHEMA_MACHINECONFIG: ResourceSchema = ResourceSchema {
    name: "MachineConfig",
    api_version: cfgd_core::API_VERSION,
    kind: "MachineConfig",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource representing a managed machine's desired and observed configuration state.",
    fields: &[
        SchemaField {
            name: "hostname",
            type_desc: "string",
            required: true,
            description: "Machine hostname",
            children: &[],
        },
        SchemaField {
            name: "profile",
            type_desc: "string",
            required: true,
            description: "Active profile name",
            children: &[],
        },
        SchemaField {
            name: "moduleRefs",
            type_desc: "[]object",
            required: false,
            description: "Modules that should be installed",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Module name",
                    children: &[],
                },
                SchemaField {
                    name: "required",
                    type_desc: "bool",
                    required: false,
                    description: "Whether the module is required (default: false)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "packages",
            type_desc: "[]string",
            required: false,
            description: "Required packages",
            children: &[],
        },
        SchemaField {
            name: "packageVersions",
            type_desc: "map[string]string",
            required: false,
            description: "Reported installed versions by package name",
            children: &[],
        },
        SchemaField {
            name: "files",
            type_desc: "[]object",
            required: false,
            description: "Managed files",
            children: &[
                SchemaField {
                    name: "path",
                    type_desc: "string",
                    required: true,
                    description: "File path on the machine",
                    children: &[],
                },
                SchemaField {
                    name: "content",
                    type_desc: "string",
                    required: false,
                    description: "Inline file content",
                    children: &[],
                },
                SchemaField {
                    name: "source",
                    type_desc: "string",
                    required: false,
                    description: "Source reference",
                    children: &[],
                },
                SchemaField {
                    name: "mode",
                    type_desc: "string",
                    required: false,
                    description: "File mode in octal (default: 0644)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "systemSettings",
            type_desc: "map[string]string",
            required: false,
            description: "System configurator settings",
            children: &[],
        },
    ],
};

static SCHEMA_CONFIGPOLICY: ResourceSchema = ResourceSchema {
    name: "ConfigPolicy",
    api_version: cfgd_core::API_VERSION,
    kind: "ConfigPolicy",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource defining fleet-wide configuration baselines. Machines are checked for compliance against policies.",
    fields: &[
        SchemaField {
            name: "name",
            type_desc: "string",
            required: true,
            description: "Policy name",
            children: &[],
        },
        SchemaField {
            name: "requiredModules",
            type_desc: "[]string",
            required: false,
            description: "Modules that must be installed",
            children: &[],
        },
        SchemaField {
            name: "packages",
            type_desc: "[]PackageRef",
            required: false,
            description: "Required packages (each entry has name and optional version constraint)",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Package name",
                    children: &[],
                },
                SchemaField {
                    name: "version",
                    type_desc: "string",
                    required: false,
                    description: "Semver version requirement (e.g. \">=1.28\", \"~2.40\")",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "settings",
            type_desc: "map[string]string",
            required: false,
            description: "Required system settings",
            children: &[],
        },
        SchemaField {
            name: "targetSelector",
            type_desc: "map[string]string",
            required: false,
            description: "Label selector to match target MachineConfigs",
            children: &[],
        },
    ],
};

static SCHEMA_DRIFTALERT: ResourceSchema = ResourceSchema {
    name: "DriftAlert",
    api_version: cfgd_core::API_VERSION,
    kind: "DriftAlert",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource created when a machine drifts from its desired state. Tracks drift details and resolution status.",
    fields: &[
        SchemaField {
            name: "deviceId",
            type_desc: "string",
            required: true,
            description: "Device identifier",
            children: &[],
        },
        SchemaField {
            name: "machineConfigRef",
            type_desc: "string",
            required: true,
            description: "Reference to the MachineConfig resource",
            children: &[],
        },
        SchemaField {
            name: "driftDetails",
            type_desc: "[]object",
            required: false,
            description: "Individual drift items",
            children: &[
                SchemaField {
                    name: "field",
                    type_desc: "string",
                    required: true,
                    description: "Field that drifted",
                    children: &[],
                },
                SchemaField {
                    name: "expected",
                    type_desc: "string",
                    required: true,
                    description: "Expected value",
                    children: &[],
                },
                SchemaField {
                    name: "actual",
                    type_desc: "string",
                    required: true,
                    description: "Actual observed value",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "severity",
            type_desc: "string",
            required: true,
            description: "Drift severity: low | medium | high | critical",
            children: &[],
        },
    ],
};

static SCHEMA_TEAMCONFIG: ResourceSchema = ResourceSchema {
    name: "TeamConfig",
    api_version: cfgd_core::API_VERSION,
    kind: "TeamConfig",
    location: "Crossplane Composite Resource (XR)",
    description: "Crossplane composite resource for team-level configuration. Fans out to per-user MachineConfig CRDs via composition function.",
    fields: &[
        SchemaField {
            name: "team",
            type_desc: "string",
            required: true,
            description: "Team name",
            children: &[],
        },
        SchemaField {
            name: "profile",
            type_desc: "string",
            required: false,
            description: "Default profile for team members",
            children: &[],
        },
        SchemaField {
            name: "source",
            type_desc: "object",
            required: false,
            description: "Team config source",
            children: &[
                SchemaField {
                    name: "url",
                    type_desc: "string",
                    required: true,
                    description: "Git URL of the team config repo",
                    children: &[],
                },
                SchemaField {
                    name: "branch",
                    type_desc: "string",
                    required: false,
                    description: "Git branch (default: master)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "modules",
            type_desc: "[]object",
            required: false,
            description: "Modules for the team",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Module name",
                    children: &[],
                },
                SchemaField {
                    name: "sourceRef",
                    type_desc: "object",
                    required: false,
                    description: "Remote module source reference",
                    children: &[
                        SchemaField {
                            name: "url",
                            type_desc: "string",
                            required: true,
                            description: "Git URL",
                            children: &[],
                        },
                        SchemaField {
                            name: "ref",
                            type_desc: "string",
                            required: false,
                            description: "Git ref (tag/commit)",
                            children: &[],
                        },
                    ],
                },
            ],
        },
        SchemaField {
            name: "policy",
            type_desc: "object",
            required: false,
            description: "Team policy settings",
            children: &[
                SchemaField {
                    name: "required",
                    type_desc: "object",
                    required: false,
                    description: "Required configuration items",
                    children: &[],
                },
                SchemaField {
                    name: "recommended",
                    type_desc: "object",
                    required: false,
                    description: "Recommended configuration items",
                    children: &[],
                },
                SchemaField {
                    name: "locked",
                    type_desc: "object",
                    required: false,
                    description: "Locked (non-overridable) items",
                    children: &[],
                },
                SchemaField {
                    name: "requiredModules",
                    type_desc: "[]string",
                    required: false,
                    description: "Modules that must be installed",
                    children: &[],
                },
                SchemaField {
                    name: "recommendedModules",
                    type_desc: "[]string",
                    required: false,
                    description: "Modules that are recommended",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "members",
            type_desc: "[]object",
            required: false,
            description: "Team members",
            children: &[
                SchemaField {
                    name: "username",
                    type_desc: "string",
                    required: true,
                    description: "Username",
                    children: &[],
                },
                SchemaField {
                    name: "sshPublicKey",
                    type_desc: "string",
                    required: false,
                    description: "SSH public key for enrollment",
                    children: &[],
                },
                SchemaField {
                    name: "profile",
                    type_desc: "string",
                    required: false,
                    description: "Profile override for this member",
                    children: &[],
                },
                SchemaField {
                    name: "hostname",
                    type_desc: "string",
                    required: false,
                    description: "Hostname override",
                    children: &[],
                },
            ],
        },
    ],
};

static ALL_SCHEMAS: &[&ResourceSchema] = &[
    &SCHEMA_MODULE,
    &SCHEMA_PROFILE,
    &SCHEMA_CONFIG,
    &SCHEMA_CONFIG_SOURCE,
    &SCHEMA_MACHINECONFIG,
    &SCHEMA_CONFIGPOLICY,
    &SCHEMA_DRIFTALERT,
    &SCHEMA_TEAMCONFIG,
];

/// Lookup table mapping user-facing names to schemas (case-insensitive).
fn find_schema(name: &str) -> Option<&'static ResourceSchema> {
    let lower = name.to_lowercase();
    ALL_SCHEMAS
        .iter()
        .find(|s| {
            s.name.to_lowercase() == lower
                || s.kind.to_lowercase() == lower
                // Additional aliases for discoverability
                || (lower == "source" && s.name == "ConfigSource")
                || (lower == "cfgd-source" && s.name == "ConfigSource")
        })
        .copied()
}

/// Walk a dot-separated field path to find nested fields.
fn resolve_field_path<'a>(
    fields: &'a [SchemaField],
    path_parts: &[&str],
) -> Option<&'a [SchemaField]> {
    if path_parts.is_empty() {
        return Some(fields);
    }
    let target = path_parts[0];
    for field in fields {
        if field.name == target {
            if path_parts.len() == 1 {
                if field.children.is_empty() {
                    // Leaf field — return it as a single-element slice
                    return Some(std::slice::from_ref(field));
                }
                return Some(field.children);
            }
            return resolve_field_path(field.children, &path_parts[1..]);
        }
    }
    None
}

fn print_field(printer: &Printer, field: &SchemaField, indent: usize, recursive: bool) {
    let prefix = " ".repeat(indent);
    let req = if field.required { " (required)" } else { "" };
    let has_children = if !field.children.is_empty() && !recursive {
        " [+]"
    } else {
        ""
    };
    printer.info(&format!(
        "{}{} <{}>{}{}",
        prefix, field.name, field.type_desc, req, has_children
    ));
    printer.info(&format!("{}  {}", prefix, field.description));

    if recursive && !field.children.is_empty() {
        for child in field.children {
            print_field(printer, child, indent + 2, true);
        }
    }
}

#[derive(Serialize)]
struct ExplainOutput {
    name: &'static str,
    api_version: &'static str,
    kind: &'static str,
    location: &'static str,
    description: &'static str,
    fields: Vec<ExplainField>,
}

#[derive(Serialize)]
struct ExplainField {
    name: &'static str,
    #[serde(rename = "type")]
    type_desc: &'static str,
    required: bool,
    description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<ExplainField>,
}

fn schema_field_to_explain(field: &SchemaField) -> ExplainField {
    ExplainField {
        name: field.name,
        type_desc: field.type_desc,
        required: field.required,
        description: field.description,
        children: field.children.iter().map(schema_field_to_explain).collect(),
    }
}

pub(super) fn cmd_explain(
    printer: &Printer,
    resource: Option<&str>,
    recursive: bool,
) -> anyhow::Result<()> {
    let resource = match resource {
        Some(r) => r,
        None => {
            if printer.is_structured() {
                let schemas: Vec<ExplainOutput> = ALL_SCHEMAS
                    .iter()
                    .map(|s| ExplainOutput {
                        name: s.name,
                        api_version: s.api_version,
                        kind: s.kind,
                        location: s.location,
                        description: s.description,
                        fields: s.fields.iter().map(schema_field_to_explain).collect(),
                    })
                    .collect();
                printer.write_structured(&schemas);
                return Ok(());
            }
            // List all available resource types
            printer.header("Available resource types");
            let rows: Vec<Vec<String>> = ALL_SCHEMAS
                .iter()
                .map(|s| {
                    vec![
                        s.name.to_string(),
                        format!("{}/{}", s.api_version, s.kind),
                        s.location.to_string(),
                    ]
                })
                .collect();
            printer.table(&["NAME", "API/KIND", "LOCATION"], &rows);
            printer.newline();
            printer.info("Use 'cfgd explain <resource>' for details");
            printer.info("Use 'cfgd explain <resource>.<field>' to drill into a field");
            printer.info("Use 'cfgd explain <resource> --recursive' for all fields expanded");
            return Ok(());
        }
    };

    // Split resource.field.path
    let parts: Vec<&str> = resource.split('.').collect();
    let resource_name = parts[0];
    let field_path = &parts[1..];

    let schema = match find_schema(resource_name) {
        Some(s) => s,
        None => {
            anyhow::bail!(
                "Unknown resource type '{}'. Run 'cfgd explain' to see available types.",
                resource_name
            );
        }
    };

    if printer.is_structured() {
        let output = ExplainOutput {
            name: schema.name,
            api_version: schema.api_version,
            kind: schema.kind,
            location: schema.location,
            description: schema.description,
            fields: schema.fields.iter().map(schema_field_to_explain).collect(),
        };
        printer.write_structured(&output);
        return Ok(());
    }

    // If there's a field path starting with "spec", skip it since we show spec fields directly
    let field_path = if !field_path.is_empty() && field_path[0] == "spec" {
        &field_path[1..]
    } else {
        field_path
    };

    if field_path.is_empty() {
        // Show resource overview + top-level fields
        printer.header(&format!("{} ({})", schema.name, schema.kind));
        printer.info(schema.description);
        printer.newline();
        printer.key_value("apiVersion", schema.api_version);
        printer.key_value("kind", schema.kind);
        printer.key_value("location", schema.location);
        printer.newline();
        printer.subheader("FIELDS (under spec):");
        printer.newline();

        for field in schema.fields {
            print_field(printer, field, 0, recursive);
        }
    } else {
        // Drill into a specific field path
        match resolve_field_path(schema.fields, field_path) {
            Some(fields) => {
                let path_str = format!(
                    "{}.spec.{}",
                    schema.name.to_lowercase(),
                    field_path.join(".")
                );
                printer.header(&path_str);

                if fields.len() == 1 && fields[0].children.is_empty() {
                    // Leaf field
                    let f = &fields[0];
                    let req = if f.required { " (required)" } else { "" };
                    printer.key_value("field", f.name);
                    printer.key_value("type", &format!("{}{}", f.type_desc, req));
                    printer.info(f.description);
                } else {
                    for field in fields {
                        print_field(printer, field, 0, recursive);
                    }
                }
            }
            None => {
                anyhow::bail!(
                    "Unknown field path '{}.{}'. Use 'cfgd explain {}' to see available fields.",
                    resource_name,
                    field_path.join("."),
                    resource_name,
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- explain tests ---

    #[test]
    fn explain_find_schema_by_kind() {
        assert!(find_schema("Module").is_some());
        assert!(find_schema("Profile").is_some());
        assert!(find_schema("Config").is_some());
        assert!(find_schema("MachineConfig").is_some());
        assert!(find_schema("ConfigPolicy").is_some());
        assert!(find_schema("DriftAlert").is_some());
        assert!(find_schema("TeamConfig").is_some());
        assert!(find_schema("ConfigSource").is_some());
    }

    #[test]
    fn explain_find_schema_case_insensitive() {
        assert!(find_schema("module").is_some());
        assert!(find_schema("PROFILE").is_some());
        assert!(find_schema("cfgdconfig").is_some());
        assert!(find_schema("configsource").is_some());
        assert!(find_schema("cfgd-source").is_some());
    }

    #[test]
    fn explain_find_schema_unknown_returns_none() {
        assert!(find_schema("nonexistent").is_none());
        assert!(find_schema("").is_none());
    }

    #[test]
    fn explain_resolve_field_path_top_level() {
        let fields = resolve_field_path(SCHEMA_MODULE.fields, &[]);
        assert!(fields.is_some());
        let fields = fields.unwrap();
        // Module has depends, packages, files, scripts
        assert!(fields.len() >= 3);
    }

    #[test]
    fn explain_resolve_field_path_nested() {
        let fields = resolve_field_path(SCHEMA_MODULE.fields, &["packages"]);
        assert!(fields.is_some());
        let children = fields.unwrap();
        // Module packages entries have name, minVersion, prefer, aliases, script, platforms
        assert!(children.len() >= 4);
    }

    #[test]
    fn explain_resolve_field_path_deep() {
        let fields = resolve_field_path(SCHEMA_PROFILE.fields, &["packages", "brew"]);
        assert!(fields.is_some());
        let children = fields.unwrap();
        // Brew has file, taps, formulae, casks
        assert_eq!(children.len(), 4);
    }

    #[test]
    fn explain_resolve_field_path_leaf() {
        let fields = resolve_field_path(SCHEMA_PROFILE.fields, &["packages", "brew", "taps"]);
        assert!(fields.is_some());
        let children = fields.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "taps");
    }

    #[test]
    fn explain_resolve_field_path_unknown() {
        let fields = resolve_field_path(SCHEMA_MODULE.fields, &["nonexistent"]);
        assert!(fields.is_none());
    }

    #[test]
    fn explain_all_schemas_have_fields() {
        for schema in ALL_SCHEMAS {
            assert!(
                !schema.fields.is_empty(),
                "Schema {} has no fields",
                schema.name
            );
            assert!(!schema.name.is_empty());
            assert!(!schema.api_version.is_empty());
            assert!(!schema.kind.is_empty());
            assert!(!schema.location.is_empty());
            assert!(!schema.description.is_empty());
        }
    }

    #[test]
    fn explain_cmd_no_args_lists_types() {
        let (printer, buf) = Printer::for_test();
        cmd_explain(&printer, None, false).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Available resource types"),
            "expected header listing resource types, got: {output}"
        );
        assert!(
            output.contains("Module"),
            "expected Module in resource list, got: {output}"
        );
        assert!(
            output.contains("Profile"),
            "expected Profile in resource list, got: {output}"
        );
        assert!(
            output.contains("CfgdConfig"),
            "expected CfgdConfig in resource list, got: {output}"
        );
    }

    #[test]
    fn explain_cmd_known_resource() {
        let (printer, buf) = Printer::for_test();
        cmd_explain(&printer, Some("module"), false).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Module"),
            "expected Module name in output, got: {output}"
        );
        assert!(
            output.contains("packages"),
            "expected packages field in module output, got: {output}"
        );
        assert!(
            output.contains("FIELDS"),
            "expected FIELDS section header, got: {output}"
        );
    }

    #[test]
    fn explain_cmd_field_path() {
        let (printer, buf) = Printer::for_test();
        cmd_explain(&printer, Some("module.packages"), false).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("module.spec.packages"),
            "expected field path header, got: {output}"
        );
        // packages has children like brew, apt, cargo etc.
        assert!(
            output.contains("brew") || output.contains("apt") || output.contains("cargo"),
            "expected package manager children in output, got: {output}"
        );
    }

    #[test]
    fn explain_cmd_spec_prefix_stripped() {
        // "module.spec.packages" should produce identical output to "module.packages"
        let (printer_a, buf_a) = Printer::for_test();
        cmd_explain(&printer_a, Some("module.packages"), false).unwrap();
        let output_a = buf_a.lock().unwrap().clone();

        let (printer_b, buf_b) = Printer::for_test();
        cmd_explain(&printer_b, Some("module.spec.packages"), false).unwrap();
        let output_b = buf_b.lock().unwrap().clone();

        assert_eq!(
            output_a, output_b,
            "spec prefix should be stripped transparently"
        );
        assert!(
            output_a.contains("module.spec.packages"),
            "expected field path header, got: {output_a}"
        );
    }

    #[test]
    fn explain_cmd_recursive() {
        let (printer, buf) = Printer::for_test();
        cmd_explain(&printer, Some("profile"), true).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Profile"),
            "expected Profile resource name, got: {output}"
        );
        // Recursive output should expand nested children (no [+] markers)
        assert!(
            !output.contains("[+]"),
            "recursive output should not have unexpanded [+] markers, got: {output}"
        );
        // Profile has nested fields like packages.brew etc. that should be expanded
        assert!(
            output.contains("inherits"),
            "expected inherits field in profile output, got: {output}"
        );
    }

    #[test]
    fn explain_cmd_unknown_resource() {
        let (printer, _buf) = Printer::for_test();
        let err = cmd_explain(&printer, Some("nonexistent"), false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown resource type") && msg.contains("nonexistent"),
            "expected unknown resource error mentioning 'nonexistent', got: {msg}"
        );
    }

    #[test]
    fn explain_cmd_unknown_field_path() {
        let (printer, _buf) = Printer::for_test();
        let err = cmd_explain(&printer, Some("module.nonexistent"), false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown field path") && msg.contains("nonexistent"),
            "expected unknown field path error mentioning 'nonexistent', got: {msg}"
        );
    }

    #[test]
    fn explain_theme_overrides_complete() {
        // ThemeOverrides has 18 fields — verify schema matches
        let fields = resolve_field_path(SCHEMA_CONFIG.fields, &["theme", "overrides"]);
        let children = fields.unwrap();
        assert_eq!(
            children.len(),
            18,
            "ThemeOverrides schema should have 18 fields, got {}",
            children.len()
        );
    }

    #[test]
    fn explain_source_alias() {
        assert!(find_schema("source").is_some());
        assert!(find_schema("cfgd-source").is_some());
        assert_eq!(find_schema("source").unwrap().name, "ConfigSource");
    }

    #[test]
    fn explain_sources_origin_has_children() {
        // sources[].origin should have drillable children
        let fields = resolve_field_path(SCHEMA_CONFIG.fields, &["sources", "origin"]);
        let children = fields.unwrap();
        assert!(
            children.len() >= 3,
            "sources.origin should have type/url/branch/auth children"
        );
    }
}
