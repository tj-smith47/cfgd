---
name: scope-audit
description: Audit cfgd plans and code for scope limitations that restrict the tool to dotfiles rather than general machine config management
allowed-tools: ["Read", "Glob", "Grep", "Bash(grep *)"]
user-invocable: true
---

## Scope Audit: Machine Config vs Dotfiles

cfgd is a machine configuration state management tool — NOT a dotfile manager. This audit checks for assumptions, language, or design decisions that unnecessarily limit scope.

### Check all plan and architecture files:

Read these files:
- `/opt/repos/cfgd/CLAUDE.md`
- `/opt/repos/cfgd/.claude/PLAN.md`
- `/opt/repos/cfgd/.claude/kubernetes-first-class.md`

### Language Audit

Search for these terms and flag them if they limit scope:
- "dotfile" / "dotfiles" — should say "config files" or "managed files"
- "home directory" as the ONLY target — should support arbitrary paths
- "developer" as the ONLY user persona — should include operators, SREs, platform engineers
- "workstation" as the ONLY target — should be clear that nodes/servers are also targets
- "personal" as the ONLY use case — should include team/organizational use

### Design Limitation Audit

Check for these patterns:
1. **$HOME assumption**: Does `files.target` default to `$HOME` with no way to target `/etc/`, `/var/`, etc.?
2. **Package manager bias**: Are only developer-focused package managers supported (brew, npm, cargo)? Are system package managers (apt, dnf, yum) treated as first-class?
3. **Privilege assumption**: Does the tool assume it runs as a regular user? Can it handle root/sudo contexts?
4. **Desktop bias**: Are notifications assumed to be desktop notifications? What about headless servers?
5. **Single-machine bias**: Does anything prevent running on multiple machines simultaneously with different roles?
6. **Config surface bias**: Is `system:` only for macOS desktop settings, or is it extensible to any system configurator?

### Report Format

For each finding:
- **Location**: file and line/section
- **Issue**: what's too narrow
- **Impact**: what use case is excluded
- **Fix**: specific change to broaden scope without losing existing functionality

End with a summary: features that are correctly scoped vs features that need broadening.
